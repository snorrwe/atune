mod config;
mod sync;

use anyhow::Context;
use clap::Parser as _;
use clap_derive::Parser;
use clap_derive::Subcommand;
use signal_hook::{
    consts::{SIGINT, SIGQUIT, SIGTERM},
    iterator::Signals,
};
use sync::sync_all_once;
use tracing::{debug, warn};
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the atune config file
    #[arg(
        long,
        short,
        env("ATUNE_CONFIG_PATH"),
        default_value("./atune.yaml"),
        value_name = "FILE"
    )]
    config: std::path::PathBuf,

    /// Path to rsync
    #[arg(long, short, env("ATUNE_RSYNC"), default_value("rsync"))]
    rsync: std::path::PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Watch,
    /// Perform all sync actions once, then exit
    SyncOnce {
        #[arg(long, short)]
        no_run_commands: bool,
    },
    /// Execute project sync once
    SyncProject {
        /// Name of the project in the config
        #[arg(long, short)]
        project: String,
        #[arg(long, short)]
        initialize: bool,

        #[clap(flatten)]
        sync_id: SyncId,
    },
}

#[derive(Debug, clap_derive::Args)]
#[group(required = true, multiple = false)]
struct SyncId {
    /// Index of the sync config inside the project
    #[arg(long)]
    index: Option<usize>,

    /// Name of the src file in the sync
    #[arg(long)]
    src: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();

    let reg = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with(tracing_subscriber::fmt::layer().with_ansi(is_tty));

    reg.try_init()?;

    let args = Args::parse();
    debug!(?args, "parsed arguments");

    let config = std::fs::OpenOptions::new()
        .read(true)
        .open(&args.config)
        .context("Failed to open config file")?;
    let mut config: config::Config =
        serde_yaml::from_reader(config).context("Failed to parse config file")?;

    for s in config.projects.values_mut().flat_map(|p| p.sync.iter_mut()) {
        s.src = std::fs::canonicalize(&s.src)
            .with_context(|| format!("Failed to canonicalize source path {}", s.src.display()))?;
    }
    debug!(?config, "Loaded config");

    match args.command {
        Command::Watch => {
            let (cancel_tx, cancel_rx) = crossbeam::channel::bounded(1);

            let h = std::thread::spawn(|| {
                crate::sync::watch(args.config, config, cancel_rx, Some(args.rsync))
            });
            match Signals::new([SIGINT, SIGTERM, SIGQUIT]) {
                Ok(mut signals) => {
                    if let Some(sig) = signals.wait().next() {
                        println!("Signal ({sig}) received. Stopping...");
                        cancel_tx.send(()).unwrap();
                        h.join()
                            .expect("Failed to join watch thread")
                            .expect("Watch error");
                        signals.handle().close();
                    }
                }
                Err(err) => {
                    warn!(?err, "Failed to register signal handler");
                }
            }
            Ok(())
        }
        Command::SyncOnce { no_run_commands } => {
            let mut config = config;
            if no_run_commands {
                for (_, p) in config.projects.iter_mut() {
                    for ele in p.sync.iter_mut() {
                        ele.on_sync.clear();
                    }
                }
            }
            sync_all_once(args.config, config)
        }
        Command::SyncProject {
            project,
            sync_id:
                SyncId {
                    index: sync_index,
                    src: sync_src,
                },
            initialize,
        } => {
            let mut config = config;

            let sync = match (sync_index, sync_src) {
                (None, Some(sync_src)) => {
                    let sync_src = std::fs::canonicalize(&sync_src).unwrap();
                    std::mem::take(
                        config
                            .projects
                            .remove(&project)
                            .with_context(|| format!("Failed to find project {project}"))?
                            .sync
                            .iter_mut()
                            .find(|s| s.src == sync_src)
                            .with_context(|| {
                                format!("Failed to find sync {}", sync_src.display())
                            })?,
                    )
                }
                (Some(sync_index), None) => std::mem::take(
                    config
                        .projects
                        .remove(&project)
                        .context("Failed to find project")?
                        .sync
                        .get_mut(sync_index)
                        .context("Failed to find sync")?,
                ),
                _ => unreachable!(),
            };

            crate::sync::execute_sync(
                &sync.try_into().context("Failed to parse sync spec")?,
                Some(args.rsync.as_os_str()),
                initialize,
            )
            .context("Failed to sync")
        }
    }
}
