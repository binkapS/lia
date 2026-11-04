//! lia — a local-first AI assistant.

use clap::Parser;

use lia::cli::{Cli, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Set up logging before anything else, so startup problems are visible.
    let log_level = if cli.verbose { "debug" } else { "info" };
    let filter = if cli.verbose {
        tracing_subscriber::EnvFilter::new(log_level)
    } else {
        // Phonon (ptts) is chatty about loading models; that noise looks bad
        // inside the terminal interface, so only lia's own info is shown.
        tracing_subscriber::EnvFilter::new("lia=info,ptts=warn,tts=warn")
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or(filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    run(cli).await
}
