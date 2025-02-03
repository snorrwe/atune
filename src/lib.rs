pub mod config;

use crate::config::Config;
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    process::{self, Stdio},
    time::Duration,
};

use anyhow::Context;
use config::FileSync;
use crossbeam::{channel, select};
use notify::Watcher;
use tracing::{debug, error, info, warn};

type FlagsParsed = Vec<String>;

#[derive(Debug)]
struct SyncOneRequest {
    path: std::path::PathBuf,
}

#[tracing::instrument]
fn execute_sync(s: &FileSync, flags: &[String], initialize: bool) -> anyhow::Result<()> {
    let status = process::Command::new("rsync")
        .args(flags.iter())
        .arg(s.src.as_os_str())
        .arg(s.dst.as_os_str())
        .spawn()
        .context("Failed to spawn sync")?
        .wait()
        .context("Failed to wait for rsync")?;

    anyhow::ensure!(status.success(), "Failed to sync files");

    debug!("Running on_sync commands");

    let run = |cmd: &str| {
        let mut proc = process::Command::new("sh")
            .arg("-s")
            .env("ATUNE_SYNC_SRC", s.src.to_string_lossy().as_ref())
            .env("ATUNE_SYNC_DST", s.dst.to_string_lossy().as_ref())
            .stdin(Stdio::piped())
            .spawn()
            .context("Failed to spawn on_sync command")?;

        let stdin = proc.stdin.as_mut().unwrap();
        stdin
            .write_all(cmd.as_bytes())
            .context("Failed to pass script via stdin")?;
        proc.wait().context("Failed to wait for process")
    };

    if initialize {
        debug!("Running init commands");
        for cmd in s.on_init.iter() {
            run(cmd)?;
        }
    }

    for cmd in s.on_sync.iter() {
        run(cmd)?;
    }
    debug!("Running on_sync commands done");
    Ok(())
}

#[tracing::instrument(skip_all)]
fn on_sync(rx: channel::Receiver<()>, commands: Vec<String>) {
    let on_sync = commands
        .iter()
        .map(|s| shell_words::split(s).unwrap())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>();

    loop {
        let Ok(_) = rx.recv() else { break };
        // collect all events received to batch updates
        for _res in rx.try_iter() {}
        debug!("running on_sync project commands");
        for cmd in on_sync.iter() {
            process::Command::new(cmd[0].as_str())
                .args(&cmd[1..])
                .spawn()
                .expect("Failed to spawn on_sync command")
                .wait()
                .unwrap();
        }
        debug!("running on_sync project commands done");
    }
    info!("on_sync disconnected");
}

#[tracing::instrument(skip_all)]
fn sync_files(
    files: Vec<(config::FileSync, FlagsParsed)>,
    rx: channel::Receiver<SyncOneRequest>,
    tx: channel::Sender<()>,
    debounce: Duration,
) {
    for (f, flags) in files.iter() {
        if let Err(err) = execute_sync(f, flags, true) {
            error!(?err, "Failed to perform initial sync");
        }
    }

    let files = files
        .iter()
        .map(|s| (std::fs::canonicalize(s.0.src.as_path()).unwrap(), s))
        .collect::<HashMap<_, _>>();

    let mut to_sync = HashSet::new();
    loop {
        let Ok(req) = rx.recv() else {
            break;
        };
        let path = &req.path;
        for a in path.ancestors() {
            if files.contains_key(a) {
                to_sync.insert(a.to_owned());
                break;
            }
        }
        std::thread::sleep(debounce);
        for req in rx.try_iter() {
            let path = &req.path;
            for a in path.ancestors() {
                if files.contains_key(a) {
                    to_sync.insert(a.to_owned());
                    break;
                }
            }
        }

        for a in to_sync.drain() {
            let (s, flags) = files[&a];
            info!(changed=?path, src=?s.src, dst=?s.dst, "syncing");
            if let Err(err) = execute_sync(s, flags, false) {
                error!(?err, "Failed to sync files");
            }
        }
        if let Err(err) = tx.send(()) {
            warn!(?err, "Failed to send on_sync event");
        }
    }
    info!("sync_files disconnected");
}

#[tracing::instrument(skip_all)]
fn watch_project<'a>(
    s: &'a std::thread::Scope<'a, '_>,
    project: config::Project,
    debounce: Duration,
    cancel: crossbeam::channel::Receiver<()>,
) -> anyhow::Result<()> {
    let mut sync = Vec::with_capacity(project.sync.len());
    for f in project.sync.into_iter() {
        let flags = if let Some(flags) = f.rsync_flags.as_deref() {
            shell_words::split(flags).context("Failed to split rsync flags")?
        } else {
            vec!["--delete".to_owned(), "-ra".to_owned()]
        };
        sync.push((f, flags));
    }

    let (tx, rx) = channel::bounded(128);

    let mut watcher = notify::recommended_watcher(tx).context("Failed to initialize watcher")?;
    for (p, _) in sync.iter() {
        debug!(path=?p, "Registering");
        let mode = if p.recursive {
            notify::RecursiveMode::Recursive
        } else {
            notify::RecursiveMode::NonRecursive
        };
        watcher
            .watch(p.src.as_path(), mode)
            .with_context(|| format!("Failed to register watcher for path {:?}", p))?;
    }

    let (one_tx, one_rx) = channel::bounded(1024);
    let (onsync_tx, onsync_rx) = channel::bounded(1024);

    s.spawn(move || sync_files(sync, one_rx, onsync_tx, debounce));
    s.spawn(move || on_sync(onsync_rx, project.on_sync));

    let mut files = HashSet::new();
    'rx: loop {
        let ev = select! {
            recv(rx) -> ev => ev,
            recv(cancel) -> _msg => break 'rx,
        };
        let Ok(Ok(ev)) = ev else {
            break 'rx;
        };
        match ev.kind {
            notify::EventKind::Create(_)
            | notify::EventKind::Modify(_)
            | notify::EventKind::Remove(_) => {
                files.extend(ev.paths);
            }
            _ => continue,
        }
        debug!(?files, "received file updates");
        for f in files.drain() {
            one_tx
                .send(SyncOneRequest { path: f })
                .expect("Failed to send");
        }
    }
    info!("filesystem watcher disconnected");
    Ok(())
}

/// Continously watch the config for changes as sync
pub fn watch(
    config: Config,
    cancel: impl Into<Option<crossbeam::channel::Receiver<()>>>,
) -> anyhow::Result<()> {
    std::thread::scope(|s| {
        let mut project_cancel = Vec::with_capacity(config.project.len());
        for project in config.project {
            let (tx, rx) = crossbeam::channel::bounded(1);
            s.spawn(move || watch_project(s, project, config.debounce, rx));
            project_cancel.push(tx);
        }

        if let Some(cancel) = cancel.into() {
            let _ = cancel.recv();
            info!("Stopping watchers");
            for tx in &project_cancel {
                if let Err(err) = tx.send(()) {
                    error!(?err, "Failed to send cancel signal to project thread");
                }
            }
        }
    });

    Ok(())
}
