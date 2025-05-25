use std::sync::Arc;

use anyhow::{Context, Result};
use base64::prelude::*;
use serde::Deserialize;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing;

#[derive(Clone)]
pub struct Github {
    host: String,
    token: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct SearchRepository {
    pub full_name: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct SearchItem {
    pub url: String,
    pub path: String,
    pub repository: SearchRepository,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize, PartialEq)]
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
    callback: Arc<(dyn Fn(SearchItem) + Send + Sync)>,
) -> Result<()> {
    tracing::debug!("starting code search task: {term}");
    let client = reqwest::Client::new();
    let url = github.host + "/search/code";

    // TODO: configure max pages
    for page in 1..=1 {
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

        let results: SearchResponse = resp.json().await?;

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
    mut rx: Receiver<String>,     // receives URL to look up
    tx: Sender<(String, String)>, // sends (URL, content)
) -> Result<()> {
    tracing::debug!("starting item content task");
    let client = reqwest::Client::new();

    loop {
        tracing::debug!("awaiting item content request");
        let Some(url) = rx.recv().await else {
            tracing::debug!("item content channel closed");
            return Ok(());
        };

        let req = client
            .request(reqwest::Method::GET, &url)
            .bearer_auth(&github.token)
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .build()?;
        tracing::debug!("sending request: {req:?}");

        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        if await_rate_limit(&resp).await? {
            continue;
        }

        let content: ContentResponse = resp.json().await?;
        let data = BASE64_STANDARD.decode(content.content.replace("\n", ""))?;
        let body = String::from_utf8(data)?;

        tracing::trace!("sending response for url {url}: {body}");
        tx.send((url, body)).await?;
    }
}

impl Github {
    pub fn new(host: String, token: String) -> Self {
        Self { host, token }
    }

    pub fn search_code(&self, term: &str, callback: Arc<(dyn Fn(SearchItem) + Sync + Send)>) {
        tracing::debug!("starting code search: {term}");
        let github = self.clone();
        let term = term.to_string();
        tokio::spawn(async move { search_code_task(github, term, callback).await.unwrap() });
    }
}

pub struct ContentClient {
    pub tx: Sender<String>,             // Sends URL
    pub rx: Receiver<(String, String)>, // Receives (URL, Content)
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

    pub async fn get_content(&self, url: impl Into<String>) -> Result<()> {
        Ok(self.tx.send(url.into()).await?)
    }

    pub async fn recv_content(&mut self) -> Option<(String, String)> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use http_test_server::http::{Method, Status};
    use http_test_server::TestServer;

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_search_code() {
        let server = TestServer::new().unwrap();

        for i in 1..4 {
            let resource = server.create_resource("/search/code");
            resource
                .status(Status::OK)
                .method(Method::GET)
                .header("x-ratelimit-remaining", "10")
                .query("page", &i.to_string())
                .query("per_page", "100")
                .query("q", "foo")
                .body_fn(move |_| {
                    std::fs::read_to_string(format!("testdata/search{i}.json")).unwrap()
                });
        }

        let github = Github::new(
            format!("http://localhost:{}", server.port()),
            "token".to_string(),
        );

        let (tx, mut rx) = mpsc::channel(8);
        github.search_code(
            "foo",
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
                },
            );
        }

        // all pages done, should close
        assert!(rx.recv().await.is_none());
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_get_content() {
        let server = TestServer::new().unwrap();

        for i in 1..4 {
            let resource = server.create_resource(&format!("/content/foo{i}"));
            resource
                .status(Status::OK)
                .method(Method::GET)
                .header("x-ratelimit-remaining", "10")
                .body_fn(move |_| {
                    format!(
                        r#"{{"content": "{}"}}"#,
                        BASE64_STANDARD.encode(format!("body{i}"))
                    )
                });
        }

        let host = format!("http://localhost:{}", server.port());
        let github = Github::new(host.clone(), "token".to_string());

        let mut content_client = ContentClient::new(github);

        let url = format!("{host}/content/foo1");
        content_client.get_content(&url).await.unwrap();
        let res = content_client.recv_content().await.unwrap();
        assert_eq!(res, (url, "body1".to_string()));

        let url = format!("{host}/content/foo2");
        content_client.get_content(&url).await.unwrap();
        let res = content_client.recv_content().await.unwrap();
        assert_eq!(res, (url, "body2".to_string()));

        let url = format!("{host}/content/foo3");
        content_client.get_content(&url).await.unwrap();
        let res = content_client.recv_content().await.unwrap();
        assert_eq!(res, (url, "body3".to_string()));
    }
}
