use crate::config::{self, CommandConfig, Config};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::PathBuf,
    process,
    time::Duration,
};

use anyhow::Context;
use crossbeam::{channel, select};
use notify::Watcher;
use tracing::{debug, error, info, warn};

#[derive(Debug)]
struct SyncOneRequest {
    path: PathBuf,
}

#[derive(Debug)]
pub struct ParsedProject {
    #[allow(unused)]
    pub name: String,
    pub sync: Vec<ParsedSync>,
    pub restart: bool,
}

#[derive(Debug)]
pub struct ParsedSync {
    pub enabled: bool,
    pub src: PathBuf,
    pub recursive: bool,
    pub dst: Option<PathBuf>,
    pub rsync_flags: Vec<String>,
    pub on_sync: Vec<CommandConfig>,
    pub on_init: Vec<CommandConfig>,
}

pub static DEFAULT_RSYCN_FLAGS: &[&str] = &["--delete", "-raPhv", "--filter", ":- .gitignore"];

impl TryFrom<config::FileSync> for ParsedSync {
    type Error = anyhow::Error;
    fn try_from(s: config::FileSync) -> Result<Self, Self::Error> {
        let mut on_sync = Vec::new();
        let mut on_init = Vec::new();

        for c in s.on_sync {
            match c.on {
                config::CommandOn::Change => on_sync.push(c),
                config::CommandOn::Init => on_init.push(c),
            }
        }

        Ok(ParsedSync {
            enabled: s.enabled,
            src: s.src,
            recursive: s.recursive,
            dst: s.dst,
            rsync_flags: if let Some(flags) = s.rsync_flags.as_deref() {
                shell_words::split(flags).context("Failed to split rsync flags")?
            } else {
                DEFAULT_RSYCN_FLAGS
                    .iter()
                    .copied()
                    .map(|x| x.to_owned())
                    .collect()
            },
            on_sync,
            on_init,
        })
    }
}

impl TryFrom<(config::ProjectName, config::Project)> for ParsedProject {
    type Error = anyhow::Error;

    fn try_from(
        (name, value): (config::ProjectName, config::Project),
    ) -> Result<Self, Self::Error> {
        let mut sync = Vec::with_capacity(value.sync.len());
        for s in value.sync {
            sync.push(s.try_into()?);
        }
        anyhow::Ok(Self {
            name,
            sync,
            restart: value.restart,
        })
    }
}

#[tracing::instrument(skip_all, fields(src))]
pub fn execute_sync(s: &ParsedSync, rsync: Option<&OsStr>, initialize: bool) -> anyhow::Result<()> {
    tracing::Span::current().record("src", s.src.display().to_string());

    let sh = xshell::Shell::new().context("Failed to init shell")?;

    if let Some(dst) = s.dst.as_ref() {
        info!("Syncing file •");

        let rsync = rsync.unwrap_or_else(|| OsStr::new("rsync"));
        let rsync_flags = s.rsync_flags.iter();
        let src = s.src.as_os_str();
        let dst = dst.as_os_str();

        let cmd = xshell::cmd!(sh, "{rsync} {rsync_flags...} {src} {dst}");
        cmd.run().context("Failed to sync files")?;
        info!("Syncing file done ✓");
    }

    let run = |cmd: &str| {
        let mut proc = xshell::cmd!(sh, "sh -s").env("ATUNE_SYNC_SRC", s.src.as_os_str());
        if let Some(dst) = s.dst.as_ref() {
            proc = proc.env("ATUNE_SYNC_DST", dst.as_os_str());
        }
        proc.stdin(cmd.as_bytes())
            .run()
            .with_context(|| format!("Command failed\n({cmd})"))
    };

    if initialize && !s.on_init.is_empty() {
        info!("Running init commands");
        for cmd in s.on_init.iter() {
            let res = run(cmd.command.as_str());
            debug!(?res, "Command result");
            if !cmd.continue_on_failure {
                res?;
            }
        }
        info!("Running init commands done");
    }

    if !s.on_sync.is_empty() {
        info!("Running on_sync commands");
        for cmd in s.on_sync.iter() {
            let res = run(cmd.command.as_str());
            debug!(?res, "Command result");
            if !cmd.continue_on_failure {
                res?;
            }
        }
        info!("Running on_sync commands done");
    }
    Ok(())
}

#[derive(Debug, Default)]
struct SyncProcesses(Vec<process::Child>);

impl Drop for SyncProcesses {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl SyncProcesses {
    pub fn cancel(&mut self) {
        // cancel in-progress syncs
        for mut proc in self.0.drain(..) {
            match proc.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    debug!("Killing in-progress sync");
                    match proc.kill() {
                        Err(err) => {
                            error!(?err, "Failed to kill sync process");
                        }
                        Ok(_) => {
                            // clean up
                            if let Err(err) = proc.wait() {
                                error!(?err, "Failed to wait for killed process");
                            }
                        }
                    }
                }
                Err(err) => {
                    error!(?err, "Failed to wait for sync command");
                }
            }
        }
    }

    pub fn wait(&mut self) {
        for mut proc in self.0.drain(..) {
            match proc.wait() {
                Ok(_) => {}
                Err(err) => {
                    error!(?err, "Failed to wait for sync command");
                }
            }
        }
    }
}

#[tracing::instrument(skip_all)]
fn sync_files(
    files: Vec<ParsedSync>,
    rx: channel::Receiver<SyncOneRequest>,
    debounce: Duration,
    config_path: &std::path::Path,
    project: &str,
    restart: bool,
) {
    let cmd = move || sync_project_cmd(project, config_path);

    let mut in_progress = SyncProcesses::default();
    for f in files.iter() {
        let proc = cmd()
            .arg("--initialize")
            .arg("--src")
            .arg(f.src.as_os_str())
            .spawn()
            .expect("Failed to spawn sync command");

        in_progress.0.push(proc);
    }

    let files = files
        .iter()
        .map(|s| (std::fs::canonicalize(s.src.as_path()).unwrap(), s))
        .collect::<HashMap<_, _>>();

    let mut to_sync = HashSet::new();
    loop {
        let Ok(req) = rx.recv() else {
            break;
        };
        let path = &req.path;
        if let Some(a) = path.ancestors().find(|a| files.contains_key(*a)) {
            to_sync.insert(a.to_owned());
        }

        if restart {
            in_progress.cancel();
        } else {
            in_progress.wait();
        }

        std::thread::sleep(debounce);
        for req in rx.try_iter() {
            if let Some(a) = req.path.ancestors().find(|a| files.contains_key(*a)) {
                to_sync.insert(a.to_owned());
            }
        }

        for a in to_sync.drain() {
            let s = files[&a];
            info!(changed=?path, src=?s.src, dst=?s.dst, "syncing");

            let proc = cmd()
                .arg("--src")
                .arg(a.as_os_str())
                .spawn()
                .expect("Failed to spawn sync command");

            in_progress.0.push(proc);
        }
    }
    info!("sync_files disconnected");
}

#[tracing::instrument(skip(project, debounce, cancel))]
fn watch_project(
    name: String,
    project: config::Project,
    debounce: Duration,
    cancel: crossbeam::channel::Receiver<()>,
    config_path: PathBuf,
    rsync: Option<PathBuf>,
) -> anyhow::Result<()> {
    let project: ParsedProject = (name, project)
        .try_into()
        .context("Failed to parse config")?;

    let (tx, rx) = channel::unbounded();

    let mut watcher =
        notify::recommended_watcher(tx.clone()).context("Failed to initialize watcher")?;

    let mut sync = project.sync;
    sync.retain(|p| p.enabled);

    for p in sync.iter() {
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

    std::thread::spawn(move || {
        sync_files(
            sync,
            one_rx,
            debounce,
            &config_path,
            project.name.as_str(),
            project.restart,
        )
    });

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
    config_path: PathBuf,
    config: Config,
    cancel: impl Into<Option<crossbeam::channel::Receiver<()>>>,
    rsync: Option<PathBuf>,
) -> anyhow::Result<()> {
    let mut project_cancel = Vec::with_capacity(config.projects.len());
    for (name, project) in config.projects {
        let (tx, rx) = crossbeam::channel::bounded(1);
        let h = std::thread::spawn({
            let config_path = config_path.clone();
            let rsync = rsync.clone();
            move || watch_project(name, project, config.debounce, rx, config_path, rsync)
        });
        project_cancel.push((tx, h));
    }
    if let Some(cancel) = cancel.into() {
        let _ = cancel.recv();
        info!("Stopping watchers");
        for (tx, _) in &project_cancel {
            if let Err(err) = tx.send(()) {
                error!(?err, "Failed to send cancel signal to project thread");
            }
        }
    }
    for (_, h) in project_cancel {
        if let Err(err) = h.join() {
            error!(?err, "Failed to join watch thread");
        }
    }

    Ok(())
}

fn sync_project_cmd(project: &str, config_path: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(
        std::env::args_os()
            .next()
            .expect("Executable name not found"),
    );
    cmd.arg("-c")
        .arg(config_path)
        .arg("sync-project")
        .arg("--project")
        .arg(project);
    cmd
}

pub fn sync_all_once(
    skip_commands: bool,
    config_path: PathBuf,
    config: Config,
) -> anyhow::Result<()> {
    let mut processes = Vec::with_capacity(config.projects.len());

    for (name, project) in config.projects {
        for f in project.sync.iter() {
            let mut cmd = sync_project_cmd(&name, &config_path);
            if skip_commands {
                cmd.arg("--no-run-commands");
            }
            let proc = cmd
                .arg("--initialize")
                .arg("--src")
                .arg(f.src.as_os_str())
                .spawn()
                .context("Failed to spawn sync command")?;

            processes.push(proc);
        }
    }
    for mut p in processes {
        if let Err(err) = p.wait() {
            error!(?err, "Sync failed");
        }
    }

    Ok(())
}
