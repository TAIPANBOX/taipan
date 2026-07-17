use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "taipan",
    version,
    about = "Native, no-Docker process supervisor for the TAIPANBOX agent-governance stack"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Bring up an environment (gateway + cloud, optionally more) and write its descriptor.
    Up(UpArgs),
    /// Stop an environment started with `taipan up`, cleanly, with no orphans.
    Down(DownArgs),
    /// Seed a synthetic demo event stream into the shared events directory.
    Demo(DemoArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Extra {
    Wardryx,
    Idryx,
}

#[derive(clap::Args)]
pub struct UpArgs {
    /// Environment name; becomes the descriptor/pidfile/keyfile filename stem.
    #[arg(long, default_value = "default")]
    pub name: String,

    /// Extra services beyond the default gateway+cloud pair, comma-separated.
    #[arg(long, value_delimiter = ',')]
    pub with: Vec<Extra>,

    /// Parent directory to look for sibling TAIPANBOX checkouts in. Defaults
    /// to the current working directory.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Gateway enforcement mode: shadow | warn | enforce.
    #[arg(long, default_value = "enforce")]
    pub gateway_mode: String,

    /// How long to wait for each service's /healthz before giving up.
    #[arg(long, default_value_t = 30)]
    pub healthz_timeout_secs: u64,

    /// Dev mode: run Cloud with the devkey fallback instead of minted keys
    /// (unblocks console auto-pairing, not for production).
    #[arg(long)]
    pub devkey: bool,
}

#[derive(clap::Args)]
pub struct DownArgs {
    #[arg(long, default_value = "default")]
    pub name: String,
}

#[derive(clap::Args)]
pub struct DemoArgs {
    #[arg(long, default_value = "default")]
    pub name: String,

    /// Number of synthetic events to append.
    #[arg(long, default_value_t = 30)]
    pub count: usize,
}
