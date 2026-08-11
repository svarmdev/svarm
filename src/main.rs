use clap::Parser;

mod cli;

use cli::Cli;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    svarm_tui::run(cli.agent, cli.path)
}
