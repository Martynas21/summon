use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = summon::cli::Cli::parse();
    summon::cli::run(args)
}
