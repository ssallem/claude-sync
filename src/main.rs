use clap::{Parser, Subcommand};

mod commands;
mod merge;
mod secrets;
mod stowignore;

#[derive(Parser)]
#[command(
    name = "claude-sync",
    version,
    about = "Sync ~/.claude/ across machines."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize sync against a remote git repository
    Init {
        /// Remote git URL (e.g. git@github.com:user/claude-config.git)
        remote: String,
    },
    /// Commit local changes and push to remote
    Push {
        /// Optional commit message
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Pull remote changes and merge into local
    Pull,
    /// Show sync status (changed files, ahead/behind)
    Status,
    /// Diagnose environment and configuration
    Doctor,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { remote } => commands::init::run(&remote),
        Command::Push { message } => commands::push::run(message.as_deref()),
        Command::Pull => commands::pull::run(),
        Command::Status => commands::status::run(),
        Command::Doctor => commands::doctor::run(),
    }
}
