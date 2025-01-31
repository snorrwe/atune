use anyhow::Context;
use clap::Parser as _;
use clap_derive::Parser;
use signal_hook::{
    consts::{SIGINT, SIGQUIT, SIGTERM},
    iterator::Signals,
};
use tracing::{debug, warn};
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, short, env("ATUNE_CONFIG_PATH"), default_value("./atune.toml"))]
    config: std::path::PathBuf,
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

    let (cancel_tx, cancel_rx) = crossbeam::channel::bounded(1);

    std::thread::spawn(move || match Signals::new([SIGINT, SIGTERM, SIGQUIT]) {
        Ok(mut signals) => {
            for sig in signals.wait() {
                println!("Signal ({sig}) received. Stopping...");
                cancel_tx.send(()).unwrap();
            }
        }
        Err(err) => {
            warn!(?err, "Failed to register ctrl+c handler");
        }
    });

    atune::run(config, cancel_rx)
}
