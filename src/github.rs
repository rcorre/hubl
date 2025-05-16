use anyhow::Result;
use serde::Deserialize;
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

impl Github {
    pub fn new(token: String) -> Github {
        Self {
            token,
            client: reqwest::Client::new(),
        }
    }

    pub async fn search_code(&self, term: &str) -> Result<SearchResponse> {
        let req = self
            .client
            .request(reqwest::Method::GET, "https://api.github.com/search/code")
            .query(&[("q", term)])
            .bearer_auth(&self.token)
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.github.v3.text-match+json",
            )
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .build()?;
        tracing::debug!("sending request: {req:?}");
        let resp = self.client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");
        let resp: SearchResponse = resp.json().await?;
        Ok(resp)
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
