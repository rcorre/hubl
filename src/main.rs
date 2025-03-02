use anyhow::Result;
use clap::Parser;
use core::str;
use serde::Deserialize;
use skim::prelude::*;

#[derive(Debug, Deserialize)]
struct SearchRepository {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    path: String,
    repository: SearchRepository,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

impl SkimItem for SearchItem {
    fn text(&self) -> Cow<str> {
        format!("{}: {}", self.repository.full_name, self.path).into()
    }
}

fn get_auth_token() -> Result<String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["auth", "token"]);
    log::debug!("executing auth command: {cmd:?}");
    let output = cmd.output()?;
    Ok(str::from_utf8(&output.stdout)?.trim().to_string())
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Query to search.
    query: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    let token = get_auth_token()?;

    let client = reqwest::Client::new();
    let req = client
        .request(reqwest::Method::GET, "https://api.github.com/search/code")
        .query(&[("q", &cli.query)])
        .bearer_auth(token)
        .header(
            reqwest::header::ACCEPT,
            "application/vnd.github.v3.text-match+json",
        )
        .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
        .build()?;
    log::debug!("sending request: {req:?}");
    let resp: SearchResponse = client.execute(req).await?.json().await?;

    log::trace!("response: {resp:?}");

    let options = SkimOptionsBuilder::default()
        .height(String::from("50%"))
        .multi(true)
        .build()
        .unwrap();

    // let input = "aaaaa\nbbbb\nccc".to_string();

    // let item_reader = SkimItemReader::default();
    // let items = item_reader.of_bufread(Cursor::new(input));
    //
    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for item in resp.items {
        let _ = tx_item.send(Arc::new(item));
    }
    drop(tx_item); // so that skim could know when to stop waiting for more items.

    // `run_with` would read and show items from the stream
    let selected_items = Skim::run_with(&options, Some(rx_item))
        .map(|out| out.selected_items)
        .unwrap_or_else(|| Vec::new());

    // for item in selected_items.iter() {
    //     println!("{}", item.output());
    // }
    Ok(())
}
