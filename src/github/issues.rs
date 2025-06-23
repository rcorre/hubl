use std::{sync::Arc, time::SystemTime};

use super::Github;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing;

const ISSUE_GRAPHQL: &str = include_str!("search.graphql");

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
struct IssueQueryVariables {
    query: String,
    count: u32,
    after: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
struct IssueQuery {
    query: String,
    variables: IssueQueryVariables,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct IssueSearchResponse {
    data: IssueSearchData,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct IssueSearchData {
    search: IssueSearchBody,
    rate_limit: RateLimit,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    end_cursor: String,
    has_next_page: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct IssueSearchBody {
    nodes: Vec<Issue>,
    issue_count: u32,
    page_info: PageInfo,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct RateLimit {
    cost: u32,
    limit: u32,
    remaining: u32,
    reset_at: chrono::DateTime<chrono::Utc>,
    used: u32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub enum IssueKind {
    #[default]
    Issue,
    PullRequest,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct User {
    pub login: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Issue {
    #[serde(rename = "__typename")]
    pub typename: IssueKind,
    pub number: u32,
    pub title: String,
    pub url: String,
    pub body: String,
    pub author: User,
}

async fn await_rate_limit(r: &RateLimit) -> Result<()> {
    tracing::debug!("ratelimit: {r:?}");

    if r.remaining > 0 {
        return Ok(());
    }

    let reset: SystemTime = r.reset_at.into();
    let duration = reset
        .duration_since(std::time::SystemTime::now())
        .unwrap_or_default();
    tracing::info!("ratelimit consumed, waiting {duration:?} until {reset:?}",);
    tokio::time::sleep(duration).await;
    Ok(())
}

async fn search_issues_task(
    github: Github,
    term: String,
    max_pages: usize,
    callback: Arc<(dyn Fn(Issue) + Send + Sync)>,
) -> Result<()> {
    tracing::debug!("starting issue search task: {term}");
    let client = reqwest::Client::new();
    let url = github.host + "/graphql";
    let mut after = "".to_string();

    for _ in 1..=max_pages {
        let req = client
            .request(reqwest::Method::POST, &url)
            .bearer_auth(&github.token)
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .json(&IssueQuery {
                query: ISSUE_GRAPHQL.to_string(),
                variables: IssueQueryVariables {
                    // TODO: &str
                    query: term.clone(),
                    count: 100,
                    after: after.clone(),
                },
            })
            .build()?;
        tracing::debug!("sending request: {req:?}");

        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        let results: IssueSearchResponse = resp.json().await?;
        tracing::trace!("parsed response: {results:#?}");

        for item in results.data.search.nodes {
            callback(item);
        }

        if !results.data.search.page_info.has_next_page {
            tracing::info!("no items remain, ending issue search");
            return Ok(());
        }

        after = results.data.search.page_info.end_cursor;
        await_rate_limit(&results.data.rate_limit).await?;
    }
    Ok(())
}

pub fn search_issues(
    github: Github,
    term: &str,
    max_pages: usize,
    callback: Arc<(dyn Fn(Issue) + Sync + Send)>,
) {
    tracing::debug!("starting issue search: {term}");
    let term = term.to_string();
    tokio::spawn(async move {
        search_issues_task(github, term, max_pages, callback)
            .await
            .unwrap()
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    use mockito::Server;
    use tokio::sync::mpsc;

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_search_issues() {
        let mut server = Server::new_async().await;

        
        let _mock1 = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::PartialJsonString(r#"{"variables":{"after":""}}"#.to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&std::fs::read_to_string("testdata/issues1.json").unwrap())
            .create_async()
            .await;
            
        let _mock2 = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::PartialJsonString(r#"{"variables":{"after":"Y3Vyc29yOjI="}}"#.to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&std::fs::read_to_string("testdata/issues2.json").unwrap())
            .create_async()
            .await;

        let github = Github {
            host: server.url(),
            token: "token".to_string(),
        };

        let (tx, mut rx) = mpsc::channel(8);
        search_issues(
            github,
            "foo",
            4,
            Arc::new(move |res| {
                tx.try_send(res).unwrap();
            }),
        );

        assert_eq!(
            rx.recv().await.unwrap(),
            Issue {
                typename: IssueKind::PullRequest,
                number: 4064,
                title: "Update README".into(),
                url: "https://github.com/octocat/Hello-World/pull/4064".into(),
                body: "Hi! I’m Momina Iqbal, a web development intern passionate about learning and building modern, responsive websites.\r\nI’m currently exploring HTML, CSS, Git, and GitHub, and working toward becoming a Full Stack Developer.".into(),
                author: User{
                login: "mominaiqbal-dev".into()
                }
            },
        );

        assert_eq!(
            rx.recv().await.unwrap(),
            Issue {
                typename: IssueKind::PullRequest,
                number: 4063,
                title: "Added index.html and index.css".into(),
                url: "https://github.com/octocat/Hello-World/pull/4063".into(),
                body: "Assignment Task".into(),
                author: User {
                    login: "mominaiqbal-dev".into()
                }
            },
        );

        assert_eq!(
            rx.recv().await.unwrap(),
            Issue {
                typename: IssueKind::PullRequest,
                number: 4062,
                title: "Corrige error en validación de login".into(),
                url: "https://github.com/octocat/Hello-World/pull/4062".into(),
                body: "Se realiza una prueba para la validación del error presentado durante el login.".into(),
                author: User {
                    login: "ricardocasta".into()
                }
            },
        );

        assert_eq!(
            rx.recv().await.unwrap(),
            Issue {
                typename: IssueKind::PullRequest,
                number: 4061,
                title: "Fixes #1: Add simple greeting function".into(),
                url: "https://github.com/octocat/Hello-World/pull/4061".into(),
                body: "This PR addresses issue #1 by adding a greetWorld() function that returns 'Hello, World!' to make the repository more welcoming to new developers.\n\n## Changes Made:\n- Added hello.js file with greetWorld() function\n- Function returns the string 'Hello, World!' as requested\n- Included JSDoc documentation for clarity\n- Added module export functionality\n- Included example usage when run directly\n\n## Testing:\nThe function can be tested by running:\n```bash\nnode hello.js\n```\n\nThis implementation makes the repository more accessible and welcoming to new developers learning to code.".into(),
                author: User {
                    login: "flazouh".into()
                }
            },
        );

        // all pages done, should close
        assert!(rx.recv().await.is_none());
    }
}
