use std::{
    path::PathBuf,
    fs::{ self, File }
};

use sqlx::prelude::*;
use atom_syndication::*;
use anyhow::{ Result, Context };
use futures::{ Stream, StreamExt };
use chrono::{ Utc, TimeZone };
use url::Url;

use tracing::info;

use crate::{ Conn, query::{ self, repo_id } };

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct Issue {
    number: i64,
    state: i64,
    title: String,
    body: String,
    user_login: String,
    html_url: String,
    updated_at: i64
}

// Naive implementation of https://www.w3.org/TR/REC-xml/#syntax
fn entity_escape(from: &str) -> String {
    let mut escaped = String::with_capacity(from.len());

    for c in from.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\'' => escaped.push_str("&apos;"),
            '"' => escaped.push_str("&quot;"),
            any => escaped.push(any)
        }
    }

    escaped
}

async fn query_issues_for_label<'conn>(conn: &'conn mut Conn,
        repo_id: i64, label: &str, only_open: bool) -> impl Stream<Item=sqlx::Result<Issue>> + 'conn {
    sqlx::query_as::<_, Issue>(r#"
        SELECT issues.number, state, title, body, user_login, html_url, updated_at FROM issues
        INNER JOIN is_labeled ON is_labeled.issue=issues.number
        WHERE is_labeled.label=(SELECT id FROM labels WHERE repo=? AND name=?)
          AND (?=0 OR issues.state=?)
        ORDER BY issues.number DESC
    "#).bind(repo_id).bind(label)
       .bind(only_open).bind(query::issues::IssueState::OPEN.to_integer())
       .fetch(conn)
}

async fn issue_to_entry(conn: &mut Conn, repo_id: i64, issue: Issue) -> Result<Entry> {
    let state_label = query::issues::IssueState::from_integer(issue.state)
        .expect("Inconsistent database, invalid issue state").to_string();
    let labels_of_issue = sqlx::query_as::<_, (String,)>(
        "SELECT labels.name FROM is_labeled
         JOIN labels ON is_labeled.label=labels.id
         JOIN issues ON (is_labeled.issue=issues.number AND is_labeled.repo=issues.repo)
         WHERE is_labeled.repo=? AND is_labeled.issue=?"
    ).bind(repo_id).bind(issue.number)
     .fetch(&mut *conn);

    let all_labels = futures::stream::iter(state_label)
        .chain(labels_of_issue
               .filter_map(|row| async { row.ok() })
               .map(|(name,)| name))
        .map(|name| Category {
            term: name,
            scheme: None,
            label: None
        })
        .collect::<Vec<_>>()
        .await;

    Ok(EntryBuilder::default()
        .title(entity_escape(&issue.title))
        .id(entity_escape(&issue.html_url))
        .updated(Utc.timestamp(issue.updated_at, 0))
        .authors(vec![
            Person {
                uri: Some(format!("https://github.com/{}", issue.user_login)),
                name: issue.user_login,
                email: None
            }
        ])
        .categories(all_labels)
        .links(vec![LinkBuilder::default()
                        .href(issue.html_url)
                        .build()
                        .expect("Failed to build link")])
        .content(ContentBuilder::default()
                    .content_type(Some(String::from("html")))
                    .value(entity_escape(&issue.body))
                    .build()
                    .expect("Failed to build content"))
        .build()
        .map_err(|err_str| anyhow::anyhow!(err_str))
        .context("Failed to build atom entry")?)
}

pub async fn generate(mut conn: &mut Conn, (ref owner, ref name): (String, String),
        out_path: PathBuf, labels: Vec<String>,
        only_open: bool) -> Result<()> {
    let labels = if labels.is_empty() {
        sqlx::query_as::<_, (String,)>(
            "SELECT name FROM labels WHERE repo=(SELECT id FROM repositories WHERE owner=? AND name=?)"
        ).bind(owner).bind(name)
         .fetch(&mut *conn)
         .filter_map(|row| async { match row {
             Ok((label,)) => Some(label),
             _ => None
         } })
         .collect()
         .await
    } else { labels };

    let repo_id = repo_id(&mut conn, owner, name).await?;

    for label in labels {
        info!("atom for {:?}", label);

        let label_url = {
            let mut url = Url::parse("https://github.com")?;
            url.path_segments_mut()
                .unwrap()
                .push(owner).push(name)
                .push("labels").push(&label);
            url.into_string()
        };

        let mut feed = FeedBuilder::default();
        feed.title(entity_escape(&label));
        feed.id(&label_url);
        feed.updated(Utc::now());
        feed.links(vec![
            LinkBuilder::default()
                .href(&label_url)
                .rel("alternate")
                .build()
                .map_err(anyhow::Error::msg)?
        ]);

        let issues: Vec<Issue> = query_issues_for_label(&mut conn, repo_id, &label, only_open).await
            .filter_map(|res| async { res.ok() })
            .collect().await;

        let entries: Vec<Entry> = {
            let mut acc = Vec::new();
            for issue in issues.into_iter() {
                acc.push(issue_to_entry(&mut conn, repo_id, issue).await?);
            }
            acc
        };
        feed.entries(entries);

        let feed = feed.build().expect("Failed to build feed");

        let feed_directory = out_path.join(label);
        fs::create_dir_all(&feed_directory)?;

        let feed_path = feed_directory.join("atom.xml");
        let mut out_file = File::create(feed_path)?;
        feed.write_to(&mut out_file)?;
    }

    Ok(())
}
