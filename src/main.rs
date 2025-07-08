use anyhow::Result;
use clap::Parser as _;
use hubl::Cli;
use hubl::{github::Github, QueryArgs};
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _};

fn get_auth_token() -> Result<String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["auth", "token"]);
    tracing::debug!("executing auth command: {cmd:?}");
    let output = cmd.output()?;
    Ok(core::str::from_utf8(&output.stdout)?.trim().to_string())
}

use std::path::Path;

fn set_repo(args: &mut QueryArgs) -> Result<()> {
    if args.repo.is_some() {
        return Ok(());
    }

    if !Path::new(".git").is_dir() {
        tracing::debug!("not a git repository");
        return Ok(());
    }

    let mut cmd = std::process::Command::new("gh");
    cmd.args([
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "--jq",
        ".nameWithOwner",
    ]);
    tracing::debug!("executing repo command: {cmd:?}");
    let output = cmd.output()?;
    let repo = core::str::from_utf8(&output.stdout)?.trim().to_string();

    tracing::debug!("setting repo: {repo}");
    args.repo = Some(repo);
    Ok(())
}

pub fn initialize_logging() -> Result<()> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
    let log_path = xdg_dirs.place_cache_file("log.txt")?;
    let log_file = std::fs::File::create(log_path)?;
    let file_subscriber = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env());
    tracing_subscriber::registry()
        .with(file_subscriber)
        .with(ErrorLayer::default())
        .init();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    initialize_logging()?;

    let cli = Cli::parse();

    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::SetCursorStyle::BlinkingBar
    )?;
    let github = Github {
        host: "https://api.github.com".to_string(),
        token: get_auth_token()?,
    };
    let result = match cli.command {
        hubl::Command::Issues(mut cmd) => {
            set_repo(&mut cmd)?;
            hubl::tui::issues::App::new(github, cmd)?
                .run(&mut terminal)
                .await
        }
    };
    ratatui::restore();
    result
}
