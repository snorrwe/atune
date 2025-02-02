pub mod config;

use crate::config::Config;
use std::{
    collections::{HashMap, HashSet},
    process,
    time::Duration,
};

use anyhow::Context;
use config::FileSync;
use crossbeam::{
    channel::{self, TryRecvError},
    select,
};
use notify::Watcher;
use tracing::{debug, error, info, warn};

type FlagsParsed = Vec<String>;

#[derive(Debug)]
struct SyncOneRequest {
    path: std::path::PathBuf,
}

#[tracing::instrument(skip_all)]
fn execute_sync<'a, S, C>(
    s: &FileSync,
    flags: &[String],
    on_sync: impl Iterator<Item = C>,
) -> anyhow::Result<()>
where
    S: std::convert::AsRef<std::ffi::OsStr>,
    C: AsRef<[S]>,
{
    let status = process::Command::new("rsync")
        .args(flags.iter())
        .arg(s.src.as_os_str())
        .arg(s.dst.as_os_str())
        .spawn()
        .context("Failed to spawn sync")?
        .wait()
        .context("Failed to wait for rsync")?;

    anyhow::ensure!(status.success(), "Failed to sync files");

    for cmd in on_sync {
        let cmd = cmd.as_ref();
        process::Command::new(cmd[0].as_ref())
            .args(&cmd[1..])
            .env("ATUNE_SYNC_SRC", s.src.to_string_lossy().as_ref())
            .env("ATUNE_SYNC_DST", s.dst.to_string_lossy().as_ref())
            .spawn()
            .expect("Failed to spawn on_sync command")
            .wait()
            .unwrap();
    }
    Ok(())
}

#[tracing::instrument(skip_all)]
fn on_sync(rx: channel::Receiver<()>, commands: Vec<String>, debounce: Duration) {
    let on_sync = commands
        .iter()
        .map(|s| shell_words::split(s).unwrap())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>();

    'rx: loop {
        let Ok(_) = rx.recv() else { break };
        debug!(?debounce, "on_sync event received, waiting...");
        std::thread::sleep(debounce);
        // collect events received while asleep to batch updates
        loop {
            match rx.try_recv() {
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'rx,
            }
        }
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
) {
    let on_sync = files
        .iter()
        .flat_map(|(s, _)| s.on_sync.iter())
        .map(|s| shell_words::split(s).unwrap())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>();

    for (f, flags) in files.iter() {
        if let Err(err) = execute_sync(f, flags, on_sync.iter()) {
            error!(?err, "Failed to perform initial sync");
        }
    }

    let files = files
        .iter()
        .map(|s| (std::fs::canonicalize(s.0.src.as_path()).unwrap(), s))
        .collect::<HashMap<_, _>>();

    loop {
        let Ok(req) = rx.recv() else {
            break;
        };
        let path = &req.path;
        for a in path.ancestors() {
            if let Some((s, flags)) = files.get(a) {
                info!(changed=?path, src=?s.src, dst=?s.dst, "syncing");
                if let Err(err) = execute_sync(s, flags, on_sync.iter()) {
                    error!(?err, "Failed to sync files");
                }
                break;
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
            Vec::new()
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

    s.spawn(move || sync_files(sync, one_rx, onsync_tx));
    s.spawn(move || on_sync(onsync_rx, project.on_sync, debounce));

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
        std::thread::sleep(debounce);

        // collect events received while asleep to batch updates
        loop {
            match rx.try_recv() {
                Ok(Ok(ev)) => {
                    files.extend(ev.paths);
                }
                Ok(Err(_)) => {
                    break 'rx;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'rx,
            }
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
pub fn run(
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
