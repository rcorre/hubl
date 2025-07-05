use std::sync::Arc;

use super::{Github, TextMatch};
use anyhow::{Context, Result};
use base64::prelude::*;
use serde::Deserialize;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SearchRepository {
    pub full_name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SearchItem {
    pub url: String,
    pub path: String,
    pub repository: SearchRepository,
    pub text_matches: Vec<TextMatch>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub items: Vec<SearchItem>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct ContentResponse {
    pub content: String,
}

// If the ratelimit is consumed, await until it is cleared
// Returns true if we were rate limited.
async fn await_rate_limit(resp: &reqwest::Response) -> Result<bool> {
    let ratelimit_remaining = resp
        .headers()
        .get("x-ratelimit-remaining")
        .context("missing x-ratelimit-remaining header")?
        .to_str()
        .context("parsing x-ratelimit-remaining header: {remaining}")?
        .parse::<usize>()
        .context("parsing x-ratelimit-remaining header: {remaining}")?;

    tracing::debug!("ratelimit remaining: {ratelimit_remaining}");

    if ratelimit_remaining == 0 {
        let reset = resp
            .headers()
            .get("x-ratelimit-reset")
            .context("missing x-ratelimit-reset header")?
            .to_str()
            .context("parsing x-ratelimit-remaining header: {remaining}")?
            .parse::<u64>()
            .context("parsing x-ratelimit-remaining header: {remaining}")?;

        let reset = std::time::UNIX_EPOCH + std::time::Duration::from_secs(reset);
        let duration = reset
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default();
        tracing::info!("ratelimit consumed, waiting {duration:?} until {reset:?}",);
        tokio::time::sleep(duration).await;
        return Ok(true);
    }

    Ok(false)
}

async fn search_code_task(
    github: Github,
    term: String,
    max_pages: usize,
    callback: Arc<(dyn Fn(SearchItem) + Send + Sync)>,
) -> Result<()> {
    tracing::debug!("starting code search task: {term}");
    let client = reqwest::Client::new();
    let url = github.host + "/search/code";

    for page in 1..=max_pages {
        let req = client
            .request(reqwest::Method::GET, &url)
            .bearer_auth(&github.token)
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .query(&[
                ("q", term.as_str()),
                ("page", page.to_string().as_str()),
                ("per_page", "100"),
            ])
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.github.v3.text-match+json",
            )
            .build()?;
        tracing::debug!("sending request: {req:?}");

        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        if await_rate_limit(&resp).await? {
            continue;
        }

        let response_text = resp.text().await?;
        let results: SearchResponse = serde_json::from_str(&response_text)
            .with_context(|| format!("Failed to parse JSON response: {response_text}"))?;

        if results.items.is_empty() {
            tracing::info!("no items remain, ending code search");
            return Ok(());
        }

        tracing::trace!("sending response: {results:?}");
        for item in results.items {
            callback(item);
        }
    }
    Ok(())
}

async fn item_content_task(
    github: Github,
    mut rx: Receiver<SearchItem>,
    tx: Sender<(SearchItem, String)>, // sends (URL, content)
) -> Result<()> {
    tracing::debug!("starting item content task");
    let client = reqwest::Client::new();

    loop {
        tracing::debug!("awaiting item content request");
        let Some(item) = rx.recv().await else {
            tracing::debug!("item content channel closed");
            return Ok(());
        };

        let req = client
            .request(reqwest::Method::GET, &item.url)
            .bearer_auth(&github.token)
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .build()?;
        tracing::debug!("sending request: {req:?}");

        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        if await_rate_limit(&resp).await? {
            continue;
        }

        let response_text = resp.text().await?;
        let content: ContentResponse = serde_json::from_str(&response_text)
            .with_context(|| format!("Failed to parse JSON response: {response_text}"))?;
        let data = BASE64_STANDARD.decode(content.content.replace("\n", ""))?;
        let body = String::from_utf8(data)?;

        tracing::trace!("sending response for url {}", item.path);
        tx.send((item, body)).await?;
    }
}

pub fn search_code(
    github: Github,
    term: &str,
    max_pages: usize,
    callback: Arc<(dyn Fn(SearchItem) + Sync + Send)>,
) {
    tracing::debug!("starting code search: {term}");
    let term = term.to_string();
    tokio::spawn(async move {
        search_code_task(github, term, max_pages, callback)
            .await
            .unwrap()
    });
}

pub struct ContentClient {
    tx: Sender<SearchItem>,             // Sends URL
    rx: Receiver<(SearchItem, String)>, // Receives (URL, Content)
}

impl ContentClient {
    pub fn new(github: Github) -> Self {
        let (req_tx, req_rx) = mpsc::channel(32);
        let (res_tx, res_rx) = mpsc::channel(32);

        tokio::spawn(async move { item_content_task(github, req_rx, res_tx).await.unwrap() });
        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    pub async fn get_content(&self, item: SearchItem) -> Result<()> {
        Ok(self.tx.send(item).await?)
    }

    pub async fn recv_content(&mut self) -> Option<(SearchItem, String)> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mockito::Server;

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_search_code() {
        let mut server = Server::new_async().await;

        let mut mocks = Vec::new();
        for page in 1..=3 {
            let mock = server
                .mock("GET", "/search/code")
                .match_query(mockito::Matcher::AllOf(vec![
                    mockito::Matcher::UrlEncoded("page".into(), page.to_string()),
                    mockito::Matcher::UrlEncoded("per_page".into(), "100".into()),
                    mockito::Matcher::UrlEncoded("q".into(), "foo".into()),
                ]))
                .with_status(200)
                .with_header("x-ratelimit-remaining", "10")
                .with_body(
                    std::fs::read_to_string(format!("testdata/search{}.json", page)).unwrap(),
                )
                .create_async()
                .await;
            mocks.push(mock);
        }

        let github = Github {
            host: server.url(),
            token: "token".to_string(),
        };

        let (tx, mut rx) = mpsc::channel(8);
        search_code(
            github,
            "foo",
            4,
            Arc::new(move |res| {
                tx.try_send(res).unwrap();
            }),
        );

        for name in ["foo", "bar", "biz", "baz"] {
            assert_eq!(
                rx.recv().await.context(format!("Awaiting {name}")).unwrap(),
                SearchItem {
                    url: format!("example.com/{name}"),
                    path: format!("{name}.txt"),
                    repository: SearchRepository {
                        full_name: format!("{name}repo"),
                    },
                    text_matches: vec![TextMatch {
                        matches: vec![crate::github::Match {
                            text: "stuff".into()
                        }]
                    }],
                },
            );
        }

        // all pages done, should close
        assert!(rx.recv().await.is_none());

        // Assert all mocks were called
        for mock in mocks {
            mock.assert_async().await;
        }
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_get_content() {
        let mut server = Server::new_async().await;

        let mut mocks = Vec::new();
        for i in 1..=3 {
            let mock = server
                .mock("GET", format!("/content/foo{}", i).as_str())
                .with_status(200)
                .with_header("x-ratelimit-remaining", "10")
                .with_body(format!(
                    r#"{{"content": "{}"}}"#,
                    BASE64_STANDARD.encode(format!("body{}", i))
                ))
                .create_async()
                .await;
            mocks.push(mock);
        }

        let host = server.url();
        let github = Github {
            host: host.clone(),
            token: "token".to_string(),
        };

        let mut content_client = ContentClient::new(github);

        let item = SearchItem {
            url: format!("{host}/content/foo1"),
            ..Default::default()
        };
        content_client.get_content(item.clone()).await.unwrap();
        let res = content_client.recv_content().await.unwrap();
        assert_eq!(res, (item, "body1".to_string()));

        let item = SearchItem {
            url: format!("{host}/content/foo2"),
            ..Default::default()
        };
        content_client.get_content(item.clone()).await.unwrap();
        let res = content_client.recv_content().await.unwrap();
        assert_eq!(res, (item, "body2".to_string()));

        let item = SearchItem {
            url: format!("{host}/content/foo3"),
            ..Default::default()
        };
        content_client.get_content(item.clone()).await.unwrap();
        let res = content_client.recv_content().await.unwrap();
        assert_eq!(res, (item, "body3".to_string()));

        // Assert all mocks were called
        for mock in mocks {
            mock.assert_async().await;
        }
    }
}
