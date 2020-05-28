use std::{
    path::PathBuf,
    fs::{ self, File }
};

use sqlx::prelude::*;
use atom_syndication::*;
use anyhow::Result;
use futures::{ Stream, StreamExt };

use tracing::info;

use crate::{ Conn, query::repo_id };

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

async fn query_issues_for_label<'conn>(conn: &'conn mut Conn, repo_id: i64, label: &str) -> impl Stream<Item=sqlx::Result<Issue>> + 'conn {
    sqlx::query_as::<_, Issue>(r#"
        SELECT issues.number, state, title, body, user_login, html_url, updated_at FROM issues
        INNER JOIN is_labeled ON is_labeled.issue=issues.number
        WHERE is_labeled.label=(SELECT id FROM labels WHERE repo=? AND name=?)
        ORDER BY issues.number DESC
    "#).bind(repo_id).bind(label)
       .fetch(conn)
}

fn issue_to_entry(issue: Issue) -> Entry {
    EntryBuilder::default()
        .title(issue.title)
        .id(issue.html_url.clone())
        .links(vec![LinkBuilder::default()
                        .href(issue.html_url)
                        .build()
                        .expect("Failed to build link")])
        .content(ContentBuilder::default()
                    .content_type(Some(String::from("html")))
                    .value(issue.body)
                    .build()
                    .expect("Failed to build content"))
        .build()
        .expect("Failed to build entry")
}

pub async fn generate(mut conn: &mut Conn, (ref owner, ref name): (String, String), out_path: PathBuf, labels: Vec<String>) -> Result<()> {
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

        let mut feed = FeedBuilder::default();
        feed.title(label.clone());

        let issues = query_issues_for_label(&mut conn, repo_id, &label).await;
        feed.entries(
            issues.filter_map(|issue| async { issue.ok() })
                  .map(issue_to_entry)
                  .collect::<Vec<_>>()
                  .await
        );

        let feed = feed.build().expect("Failed to build feed");

        let feed_directory = out_path.join(label);
        fs::create_dir_all(&feed_directory)?;

        let feed_path = feed_directory.join("atom.xml");
        let mut out_file = File::create(feed_path)?;
        feed.write_to(&mut out_file)?;
    }

    Ok(())
}
