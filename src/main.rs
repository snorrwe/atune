use anyhow::Context;
use clap::Parser as _;
use clap_derive::Parser;
use clap_derive::Subcommand;
use signal_hook::{
    consts::{SIGINT, SIGQUIT, SIGTERM},
    iterator::Signals,
};
use tracing::{debug, warn};
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(
        long,
        short,
        env("ATUNE_CONFIG_PATH"),
        default_value("./atune.toml"),
        value_name = "FILE"
    )]
    config: std::path::PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Watch,
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

    let config = std::fs::read_to_string(args.config).context("Failed to read config file")?;
    let config = toml::from_str(&config).context("Failed to parse config file")?;

    debug!(?config, "loaded config");

    match args.command {
        Command::Watch => {
            let (cancel_tx, cancel_rx) = crossbeam::channel::bounded(1);

            let h = std::thread::spawn(|| atune::watch(config, cancel_rx));
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
    }
}
