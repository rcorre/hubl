use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc::{Receiver, Sender};
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
pub struct ContentResponse {
    pub content: String,
}

async fn search_code_task(github: Github, term: String, tx: Sender<SearchResponse>) -> Result<()> {
    tracing::debug!("starting code search task: {term}");
    let client = reqwest::Client::new();
    let mut page = 1;
    let url = github.host + "/search/code";

    loop {
        let req = client
            .request(reqwest::Method::GET, &url)
            .query(&[
                ("q", term.as_str()),
                ("page", page.to_string().as_str()),
                ("per_page", "100"),
            ])
            .bearer_auth(&github.token)
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.github.v3.text-match+json",
            )
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .build()?;
        tracing::debug!("sending request: {req:?}");
        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        let ratelimit_remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .context("missing x-ratelimit-remaining header")?
            .to_str()
            .context("parsing x-ratelimit-remaining header: {remaining}")?
            .parse::<usize>()
            .context("parsing x-ratelimit-remaining header: {remaining}")?;

        tracing::debug!("ratelimit remaining: {ratelimit_remaining}");

        let results: SearchResponse = resp.json().await?;

        if results.items.is_empty() {
            tracing::info!("no items remain, ending code search");
            return Ok(());
        }

        tracing::trace!("sending response: {results:?}");
        tx.send(results).await?;

        if ratelimit_remaining == 0 {
            // TODO: check x-ratelimit-reset, wait until we can query again
            tracing::info!("ratelimit consumed, ending code search");
            return Ok(());
        }

        page += 1;
    }
}

impl Github {
    pub fn new(host: String, token: String) -> Github {
        Self { host, token }
    }

    pub fn search_code(&self, term: &str) -> Receiver<SearchResponse> {
        tracing::debug!("starting code search: {term}");
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let github = self.clone();
        let term = term.to_string();
        tokio::spawn(async move { search_code_task(github, term, tx).await.unwrap() });
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

        let mut rx = github.search_code("foo");

        // page 1
        let res = rx.recv().await.unwrap();
        let expected = SearchResponse {
            items: vec![
                SearchItem {
                    url: "example.com/foo".into(),
                    path: "foo.txt".into(),
                    repository: SearchRepository {
                        full_name: "foorepo".into(),
                    },
                },
                SearchItem {
                    url: "example.com/bar".into(),
                    path: "bar.txt".into(),
                    repository: SearchRepository {
                        full_name: "barrepo".into(),
                    },
                },
            ],
        };
        assert_eq!(res, expected);

        // page 2
        let res = rx.recv().await.unwrap();
        let expected = SearchResponse {
            items: vec![
                SearchItem {
                    url: "example.com/baz".into(),
                    path: "baz.txt".into(),
                    repository: SearchRepository {
                        full_name: "bazrepo".into(),
                    },
                },
                SearchItem {
                    url: "example.com/biz".into(),
                    path: "biz.txt".into(),
                    repository: SearchRepository {
                        full_name: "bizrepo".into(),
                    },
                },
            ],
        };
        assert_eq!(res, expected);

        // all pages done, should close
        assert!(rx.recv().await.is_none());
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_search_code_ratelimit() {
        let server = TestServer::new().unwrap();

        server
            .create_resource("/search/code")
            .status(Status::OK)
            .method(Method::GET)
            .header("x-ratelimit-remaining", "0")
            .query("page", "1")
            .query("per_page", "100")
            .query("q", "foo")
            .body_fn(move |_| std::fs::read_to_string("testdata/search1.json").unwrap());

        let github = Github::new(
            format!("http://localhost:{}", server.port()),
            "token".to_string(),
        );

        let mut rx = github.search_code("foo");

        // page 1
        let res = rx.recv().await.unwrap();
        let expected = SearchResponse {
            items: vec![
                SearchItem {
                    url: "example.com/foo".into(),
                    path: "foo.txt".into(),
                    repository: SearchRepository {
                        full_name: "foorepo".into(),
                    },
                },
                SearchItem {
                    url: "example.com/bar".into(),
                    path: "bar.txt".into(),
                    repository: SearchRepository {
                        full_name: "barrepo".into(),
                    },
                },
            ],
        };
        assert_eq!(res, expected);

        // ratelimit reached, should close
        assert!(rx.recv().await.is_none());
    }
}
