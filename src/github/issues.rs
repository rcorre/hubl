use std::{sync::Arc, time::SystemTime};

use super::Github;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing;

const ISSUE_GRAPHQL: &str = include_str!("search.graphql");

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
struct IssueQueryVariables {
    query: String,
    page_size: u32,
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
struct IssueSearchData {
    search: IssueSearchBody,
    rate_limit: RateLimit,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct PageInfo {
    end_cursor: String,
    has_next_page: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct IssueSearchBody {
    nodes: Vec<Issue>,
    issue_count: u32,
    page_info: PageInfo,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
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
    login: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Issue {
    typename: IssueKind,
    number: String,
    title: String,
    url: String,
    body: String,
    author: User,
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
    let after = "".to_string();

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
                    page_size: 100,
                    after: after.clone(),
                },
            })
            .build()?;
        tracing::debug!("sending request: {req:?}");

        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        let results: IssueSearchResponse = resp.json().await?;

        tracing::trace!("sending response: {results:?}");
        for item in results.data.search.nodes {
            callback(item);
        }

        if !results.data.search.page_info.has_next_page {
            tracing::info!("no items remain, ending issue search");
            return Ok(());
        }

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
