use clap::Parser;
use svarm::cli::Cli;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    svarm::runtime::run(cli.agent, cli.path)
}
