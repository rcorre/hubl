use clap::{Args, Parser, Subcommand};

pub mod code;
pub mod github;
pub mod preview;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Search code.
    Code(QueryArgs),
}

#[derive(Args, Default)]
pub struct QueryArgs {
    /// Query to search.
    pub query: String,

    /// Maximum number of result pages.
    #[arg(short, long, default_value_t = 5)]
    pub pages: usize,
}
