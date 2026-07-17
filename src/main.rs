mod buildutil;
mod cli;
mod commands;
mod descriptor;
mod health;
mod home;
mod keys;
mod pidfile;
mod procutil;
mod services;
mod util;
mod workspace;

use clap::Parser;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = cli::Cli::parse();
    let result = match cli.command {
        cli::Command::Up(args) => commands::up::run(args),
        cli::Command::Down(args) => commands::down::run(args),
        cli::Command::Demo(args) => commands::demo::run(args),
    };

    if let Err(e) = result {
        eprintln!("taipan: error: {e:#}");
        std::process::exit(1);
    }
}
