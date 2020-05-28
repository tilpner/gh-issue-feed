use sqlx::prelude::*;
use anyhow::{ Result, Context };

use crate::Conn;

pub mod issues;
pub mod labels;

#[derive(sqlx::FromRow, sqlx::Type)]
pub struct RepositoryInfo {
    pub owner: String,
    pub name: String,
    pub label_count: i64,
    pub issue_count: i64
}

pub async fn repo_id(conn: &mut Conn, owner: &str, name: &str) -> Result<i64> {
    sqlx::query_as::<_, (i64,)>(
        "INSERT OR IGNORE INTO repositories (owner, name) VALUES (?, ?);
         SELECT id FROM repositories WHERE owner = ? AND name = ?"
    ).bind(owner).bind(name)
     .bind(owner).bind(name)
     .fetch_one(conn)
     .await
     .map(|(id,)| id)
     .with_context(|| format!("Couldn't find repo '{}/{}' in database", owner, name))
}

async fn last_updated(conn: &mut Conn, repo: i64) -> Result<Option<i64>> {
    sqlx::query_as::<_, (i64,)>(
        "SELECT MAX(updated_at) FROM issues WHERE repo = ?",
    ).bind(repo)
     .fetch_optional(conn)
     .await
     .map(|opt| opt.map(|row| row.0))
     .with_context(|| format!("Couldn't find time of last update for repo id {}", repo))
}

pub async fn list_repositories(db: &mut Conn) -> sqlx::Result<Vec<RepositoryInfo>> {
    sqlx::query_as(
        "SELECT repositories.owner, repositories.name,
            (SELECT count(id) FROM labels WHERE repo = repositories.id) AS label_count,
            (SELECT count(number) FROM issues WHERE repo = repositories.id) AS issue_count
         FROM repositories"
    ).fetch_all(db)
     .await
}

pub mod graphql {
    use std::time::Duration;
    use reqwest::header;
    use serde::Serialize;
    use futures_retry::{ ErrorHandler, RetryPolicy, FutureRetry };
    use graphql_client::QueryBody;

    static API_ENDPOINT: &str = "https://api.github.com/graphql";
    static USER_AGENT: &str = "github.com/tilpner/github-label-feed";

    static RETRY_DELAY: &[u64] = &[ 5, 50, 250, 1000, 5000, 25000 ];

    pub struct RetryStrategy;
    impl ErrorHandler<reqwest::Error> for RetryStrategy {
        type OutError = reqwest::Error;

        fn handle(&mut self, attempt: usize, e: reqwest::Error) -> RetryPolicy<Self::OutError> {
            match RETRY_DELAY.get(attempt) {
                Some(&ms) => RetryPolicy::WaitRetry(Duration::from_millis(ms)),
                None => RetryPolicy::ForwardError(e)
            }
        }
    }

    pub async fn query(client: &reqwest::Client, api_token: &str, query: QueryBody<impl Serialize>) -> reqwest::Result<reqwest::Response> {
        FutureRetry::new(|| {
            client
                .post(API_ENDPOINT)
                .timeout(Duration::from_secs(60))
                .header(header::USER_AGENT, USER_AGENT)
                .bearer_auth(api_token)
                .json(&query)
                .send()
        }, RetryStrategy)
            .await
            .map(|(res, _)| res)
            .map_err(|(e, _)| e)
    }
}
