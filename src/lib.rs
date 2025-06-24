use clap::{Args, Parser, Subcommand};

pub mod github;
pub mod tui;

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

    /// Search issues.
    Issues(QueryArgs),
}

#[derive(Args, Default)]
pub struct QueryArgs {
    /// Query to search.
    pub query: String,

    /// Maximum number of result pages.
    #[arg(short, long, default_value_t = 5)]
    pub pages: usize,

    /// Repository to search.
    /// Defaults to the current repository, or all repos if the current directory is not a git repository.
    /// Pass an empty string to search all repositories.
    #[arg(short, long)]
    pub repo: Option<String>,
}

impl QueryArgs {
    pub fn to_query(&self) -> String {
        match &self.repo {
            Some(repo) => format!("repo:{repo} {}", self.query),
            None => self.query.clone(),
        }
    }
}

#[test]
fn test_to_query() {
    let qa = QueryArgs {
        query: "foo".into(),
        repo: None,
        ..Default::default()
    };
    assert_eq!(qa.to_query(), "foo");

    let qa = QueryArgs {
        query: "foo".into(),
        repo: Some("bar/baz".into()),
        ..Default::default()
    };
    assert_eq!(qa.to_query(), "repo:bar/baz foo");
}
