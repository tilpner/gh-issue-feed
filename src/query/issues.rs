#![allow(proc_macro_derive_resolution_fallback)]

use graphql_client::{ GraphQLQuery, Response };
use reqwest::Client;

use chrono::{ Utc, TimeZone };
use tracing::{ error, info, debug };

use crate::{ Conn, query::* };

type URI = String;
type HTML = String;
type DateTime = String;

#[derive(GraphQLQuery)]
#[graphql(
    // curl https://api.github.com/graphql -H 'Authorization: bearer ...'
    schema_path = "graphql/github.json",
    query_path = "graphql/issues.graphql",
    response_derives = "Debug"
)]
pub struct IssuesQuery;

pub use issues_query::IssueState;
impl IssueState {
    pub fn from_integer(i: i64) -> Option<Self> {
        match i {
            0 => Some(Self::OPEN),
            1 => Some(Self::CLOSED),
            _ => None
        }
    }

    pub fn to_integer(&self) -> i64 {
        match self {
            Self::OPEN => 0,
            Self::CLOSED => 1,
            Self::Other(_) => 2
        }
    }

    pub fn to_string(&self) -> Option<String> {
        match self {
            Self::OPEN => Some("open"),
            Self::CLOSED => Some("closed"),
            Self::Other(_) => None
        }.map(str::to_owned)
    }
}


pub async fn update(mut conn: &mut Conn, github_api_token: &str, (ref owner, ref name): (String, String)) -> anyhow::Result<()> {
    let repo = repo_id(conn, owner, name).await?;

    let last_updated = last_updated(conn, repo)
        .await?
        .map(|t| Utc.timestamp(t, 0).to_rfc3339());
    info!("updating repo {}/{} ({}), last update from {:?}", owner, name, repo, last_updated);

    let client = Client::new();

    let mut has_next_page = true;
    let mut last_cursor = None;
    while has_next_page {
        eprint!(".");
        let query = IssuesQuery::build_query(issues_query::Variables {
            owner: owner.to_owned(),
            name: name.to_owned(),
            since: last_updated.clone(),
            after: last_cursor.clone()
        });

        let res = graphql::query(&client, github_api_token, query).await?;
        let response: Response<issues_query::ResponseData> = res.json().await?;

        for error in response.errors.unwrap_or_default() {
            error!("{:?}", error);
        }

        let repository = response.data
            .expect("Missing response data")
            .repository
            .expect("Missing repository");
    
        has_next_page = repository.issues.page_info.has_next_page;
        debug!("has_next_page: {}", has_next_page);
        let issues = repository.issues.edges.unwrap_or_default();

        for issue in issues.into_iter().flatten() {
            last_cursor = Some(issue.cursor);
            if let Some(issue) = issue.node {
                debug!("#{}: {}", issue.number, issue.title);
                let ts = chrono::DateTime::parse_from_rfc3339(&issue.updated_at)
                    .expect("failed to parse datetime")
                    .timestamp();
                let author = issue.author
                    .map(|author| author.login)
                    .unwrap_or_else(|| String::from("ghost"));

                sqlx::query(
                    "REPLACE INTO issues (repo, number, state, title, body, user_login, html_url, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                ).bind(repo).bind(issue.number)
                 .bind(issue.state.to_integer()).bind(issue.title).bind(issue.body_html)
                 .bind(author).bind(issue.url).bind(ts)
                 .execute(&mut conn)
                 .await?;

                sqlx::query(
                    "DELETE FROM is_labeled WHERE repo=? AND issue=?"
                ).bind(repo).bind(issue.number)
                 .execute(&mut conn)
                 .await?;

                let labels = issue.labels
                    .map(|l| l.edges)
                    .unwrap_or_default()
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                    .map(|l| l.node)
                    .flatten();

                for label in labels {
                    debug!("label: {}", label.name);
                    sqlx::query(
                        "INSERT INTO is_labeled (repo, issue, label) VALUES (?, ?, (SELECT id FROM labels WHERE name=?))"
                    ).bind(repo).bind(issue.number).bind(label.name)
                     .execute(&mut conn)
                     .await?;
                }
            }
        }
    }

    Ok(())
}
