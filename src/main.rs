use anyhow::{Context, Result};
use base64::prelude::*;
use clap::Parser;
use core::str;
use nucleo::Nucleo;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct SearchRepository {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    url: String,
    path: String,
    repository: SearchRepository,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct ContentResponse {
    content: String,
}

// async fn item_preview(item: &SearchItem) -> Result<Vec<AnsiString<'static>>> {
//     let client = reqwest::Client::new();
//     let req = client
//         .request(reqwest::Method::GET, &item.url)
//         .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
//         .build()?;
//     let content: ContentResponse = client.execute(req).await?.json().await?;
//     let text = match BASE64_STANDARD.decode(content.content.replace("\n", "")) {
//         Ok(s) => String::from_utf8(s).unwrap(),
//         Err(e) => e.to_string(),
//     };
//     let lines = text.lines().map(|l| l.to_owned().into()).collect();
//     Ok(lines)
// }

// async fn preview(items: Vec<Arc<dyn SkimItem>>) -> Vec<AnsiString<'static>> {
//     // items
//     //     .iter()
//     //     .map(|x| {
//     //         (**x)
//     //             .as_any()
//     //             .downcast_ref::<SearchItem>()
//     //             .unwrap()
//     //             .url
//     //             .clone()
//     //             .into()
//     //     })
//     //     .collect()

//     let Some(item) = items.first() else {
//         return vec![];
//     };
//     let item = (**item).as_any().downcast_ref::<SearchItem>().unwrap();

//     let lines = match item_preview(item).await {
//         Ok(lines) => lines,
//         Err(err) => vec![err.to_string().into()],
//     };
//     lines
// }

// fn get_auth_token() -> Result<String> {
//     let mut cmd = std::process::Command::new("gh");
//     cmd.args(["auth", "token"]);
//     log::debug!("executing auth command: {cmd:?}");
//     let output = cmd.output()?;
//     Ok(str::from_utf8(&output.stdout)?.trim().to_string())
// }

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Query to search.
    query: String,
}

fn redraw() {}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    // let cli = Cli::parse();

    // let token = get_auth_token()?;

    // let client = reqwest::Client::new();
    // let req = client
    //     .request(reqwest::Method::GET, "https://api.github.com/search/code")
    //     .query(&[("q", &cli.query)])
    //     .bearer_auth(token)
    //     .header(
    //         reqwest::header::ACCEPT,
    //         "application/vnd.github.v3.text-match+json",
    //     )
    //     .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
    //     .build()?;
    // log::debug!("sending request: {req:?}");
    // let resp: SearchResponse = client.execute(req).await?.json().await?;

    // log::trace!("response: {resp:?}");

    let mut nucleo = Nucleo::new(nucleo::Config::DEFAULT, Arc::new(redraw), None, 1);

    let injector = nucleo.injector();
    injector.push("foo", |item, columns| {});
    nucleo.tick(10);
    let snap = nucleo.snapshot();
    for item in snap.matched_items(0..snap.matched_item_count()) {
        eprintln!("{}", item.data);
    }

    // let input = "aaaaa\nbbbb\nccc".to_string();

    // for item in selected_items.iter() {
    //     println!("{}", item.output());
    // }
    Ok(())
}
