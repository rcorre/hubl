use std::time::SystemTime;

use super::Github;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct IssueSearchError {
    message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
enum IssueSearchResponse {
    Ok { data: IssueSearchData },
    Err { errors: Vec<IssueSearchError> },
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
    edges: Vec<IssueEdge>,
    issue_count: u32,
    page_info: PageInfo,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct IssueEdge {
    node: Issue,
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
    pub author: Option<User>,
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
    mut recv: mpsc::Receiver<u32>,
    send: mpsc::Sender<Vec<Issue>>,
) -> Result<()> {
    tracing::debug!("starting issue search task: {term}");
    let client = reqwest::Client::new();
    let url = github.host + "/graphql";
    let mut after = "".to_string();

    while let Some(count) = recv.recv().await {
        let req = client
            .request(reqwest::Method::POST, &url)
            .bearer_auth(&github.token)
            .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
            .json(&IssueQuery {
                query: ISSUE_GRAPHQL.to_string(),
                variables: IssueQueryVariables {
                    // TODO: &str
                    query: term.clone(),
                    count,
                    after: after.clone(),
                },
            })
            .build()?;
        tracing::debug!("sending request: {req:?}");

        let resp = client.execute(req).await?;
        tracing::trace!("got response: {resp:?}");

        let response_text = resp.text().await?;
        let results: IssueSearchResponse = serde_json::from_str(&response_text)
            .with_context(|| format!("Failed to parse JSON response: {response_text}"))?;
        tracing::trace!("parsed response: {results:#?}");

        let data = match results {
            IssueSearchResponse::Ok { data } => data,
            IssueSearchResponse::Err { errors } => bail!("Issue search failed: {errors:?}"),
        };

        // TODO: get issues instead of nodes
        send.send(data.search.edges.into_iter().map(|x| x.node).collect())
            .await?;

        if !data.search.page_info.has_next_page {
            tracing::info!("no items remain, ending issue search");
            return Ok(());
        }

        after = data.search.page_info.end_cursor;
        await_rate_limit(&data.rate_limit).await?;
    }
    Ok(())
}

// Start searching for issues.
// recv sends a request for N issues
// send sends the results for that request
pub fn search_issues(
    github: Github,
    term: &str,
    recv: mpsc::Receiver<u32>,
    send: mpsc::Sender<Vec<Issue>>,
) {
    tracing::debug!("starting issue search: {term}");
    let term = term.to_string();
    tokio::spawn(async move { search_issues_task(github, term, recv, send).await.unwrap() });
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

        let mock_configs = [
            ("", "testdata/issues1.json"),
            ("Y3Vyc29yOjI=", "testdata/issues2.json"),
        ];

        let mut mocks = Vec::new();
        for (after, file) in mock_configs {
            let mock = server
                .mock("POST", "/graphql")
                .match_body(mockito::Matcher::PartialJsonString(format!(
                    r#"{{"variables":{{"after":"{}"}}}}"#,
                    after
                )))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(std::fs::read_to_string(file).unwrap())
                .create_async()
                .await;
            mocks.push(mock);
        }

        let github = Github {
            host: server.url(),
            token: "token".to_string(),
        };

        let (recv_tx, recv_rx) = mpsc::channel(8);
        let (resp_tx, mut resp_rx) = mpsc::channel(8);
        search_issues(github, "foo", recv_rx, resp_tx);

        recv_tx.send(2).await.unwrap();
        assert_eq!(
            resp_rx.recv().await.unwrap(),
            vec![
                Issue {
                    typename: IssueKind::Issue,
                    number: 3556,
                    title: "LICENSE-CODE".into(),
                    url: "https://github.com/octocat/Hello-World/issues/3556".into(),
                    body: "".into(),
                    author: Some(User {
                        login: "dikehtaw".into()
                    })
                },
                Issue {
                    typename: IssueKind::Issue,
                    number: 3564,
                    title: "CODE OF. THE ICENSES".into(),
                    url: "https://github.com/octocat/Hello-World/issues/3564".into(),
                    body: "[interviews.docx](https://github.com/user-attachments/files/18794937/interviews.docx)".into(),
                    author: Some(User {
                        login: "reesecooper121".into()
                    })
                },
            ]
        );

        recv_tx.send(2).await.unwrap();
        assert_eq!(
            resp_rx.recv().await.unwrap(),
            vec![
                Issue {
                    typename: IssueKind::Issue,
                    number: 2356,
                    title: "Terraform AWS CODE".into(),
                    url: "https://github.com/octocat/Hello-World/issues/2356".into(),
                    body: "terraform {\n  required_providers {\n    aws = {\n      source  = \"hashicorp/aws\"\n      version = \"~> 4.0\"\n    }\n  }\n}\n\n# Configure the AWS Provider\nprovider \"aws\" {\n  region = \"us-east-1\"\n}\n\n# Create a VPC\nresource \"aws_vpc\" \"example\" {\n  cidr_block = \"10.0.0.0/16\"\n} provider \"aws\" {\n  shared_config_files      = [\"/Users/tf_user/.aws/conf\"]\n  shared_credentials_files = [\"/Users/tf_user/.aws/creds\"]\n  profile                  = \"customprofile\"\n} provider \"aws\" {\n  assume_role {\n    role_arn                = \"arn:aws:iam::123456789012:role/ROLE_NAME\"\n    session_name            = \"SESSION_NAME\"\n    web_identity_token_file = \"/Users/tf_user/secrets/web-identity-token\"\n  }\n} provider \"aws\" {\n  profile = \"customprofile\"\n} export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\nexport AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\nexport AWS_DEFAULT_REGION=us-west-2 export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\nexport AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\nexport AWS_DEFAULT_REGION=us-west-2 $ export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n$ export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n$ export AWS_DEFAULT_REGION=us-west-2 $Env:<variable-name> = \"<new-value>\"Get-Member : You must specify an object for the Get-Member cmdlet.\nAt line:1 char:12\n+ $env:foo | Get-Member\n+            ~~~~~~~~~~\n    + CategoryInfo          : CloseError: (:) [Get-Member], InvalidOperationException\n    + FullyQualifiedErrorId : NoObjectInGetMember,Microsoft.PowerShell.Commands.GetMemberCommand $Env:CompanyUri = 'https://internal.contoso.com'\n$Env:Path += ';C:\\Tools'4 $Env:CompanyUri = 'https://internal.contoso.com'\n$Env:Path += ';C:\\Tools'".into(),
                    author: Some(User {
                        login: "hitesh7353871909".into()
                    })
                },
                Issue {
                    typename: IssueKind::PullRequest,
                    number: 2648,
                    title: "changed a bit of code".into(),
                    url: "https://github.com/octocat/Hello-World/pull/2648".into(),
                    body: "i made changes".into(),
                    author: Some(User {
                        login: "codeblue1230".into()
                    })
                },
            ]
        );

        // all pages done, should close
        assert!(resp_rx.recv().await.is_none());

        // Assert all mocks were called
        for mock in mocks {
            mock.assert_async().await;
        }
    }
}
