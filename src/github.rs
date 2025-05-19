use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing;

pub struct Github {
    token: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct SearchRepository {
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchItem {
    pub url: String,
    pub path: String,
    pub repository: SearchRepository,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
pub struct ContentResponse {
    pub content: String,
}

async fn search_code_task(token: String, term: String, tx: Sender<SearchResponse>) -> Result<()> {
    let client = reqwest::Client::new();
    let mut page = 1;

    // TODO: loop until total page count
    loop {
        let req = client
            .request(reqwest::Method::GET, "https://api.github.com/search/code")
            .query(&[
                ("q", term.as_str()),
                ("page", page.to_string().as_str()),
                ("per_page", "100"),
            ])
            .bearer_auth(&token)
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.github.v3.text-match+json",
            )
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .build()?;
        tracing::debug!("sending request: {req:?}");
        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .context("missing x-ratelimit-remaining header")?
            .to_str()
            .context("parsing x-ratelimit-remaining header: {remaining}")?
            .parse::<usize>()
            .context("parsing x-ratelimit-remaining header: {remaining}")?;

        tracing::debug!("ratelimit remaining: {remaining}");

        let results: SearchResponse = resp.json().await?;
        tracing::trace!("sending response: {results:?}");
        tx.send(results).await?;

        if remaining == 0 {
            // TODO: check x-ratelimit-reset, wait until we can query again
            tracing::info!("ratelimit consumed, ending code search");
            return Ok(());
        }

        page += 1;
    }
}

impl Github {
    pub fn new(token: String) -> Github {
        Self {
            token,
            client: reqwest::Client::new(),
        }
    }

    // page should start at 1
    pub fn search_code(&self, term: &str) -> Receiver<SearchResponse> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let token = self.token.clone();
        let term = term.to_string();
        tokio::spawn(async move { search_code_task(token, term, tx).await });
        rx
    }

    pub async fn get_item_content(item: &SearchItem) -> Result<String> {
        use base64::prelude::*;

        let client = reqwest::Client::new();
        let req = client
            .request(reqwest::Method::GET, &item.url)
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .build()?;
        let content: ContentResponse = client.execute(req).await?.json().await?;
        let data = BASE64_STANDARD.decode(content.content.replace("\n", ""))?;
        Ok(String::from_utf8(data)?)
    }
}
