use std::{
    fs::{ self, File }
};

use sqlx::prelude::*;
use anyhow::{ Result, Context };
use futures::{ Stream, StreamExt };
use chrono::{ Utc, TimeZone };
use url::Url;

use tracing::info;

use crate::{
    parse_repo,
    Conn, GenerateOpts,
    query::{ self, repo_id }
};

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
fn xml_entity_escape(from: &str) -> String {
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

fn path_escape(from: &str) -> String {
    from.replace('/', "_")
        .replace(char::is_whitespace, "_")
}

async fn query_issues_for_label<'conn>(conn: &'conn mut Conn,
        repo_id: i64, label: &str, state_mask: i64) -> impl Stream<Item=sqlx::Result<Issue>> + 'conn {
    sqlx::query_as::<_, Issue>(r#"
        SELECT issues.number, state, title, body, user_login, html_url, updated_at FROM issues
        INNER JOIN is_labeled ON is_labeled.issue=issues.number
        WHERE is_labeled.label=(SELECT id FROM labels WHERE repo=? AND name=?)
          AND issues.state & ? != 0
        ORDER BY issues.number DESC
    "#).bind(repo_id).bind(label)
       .bind(state_mask)
       .fetch(conn)
}

async fn issue_to_atom_entry(issue: &Issue, labels: &[String]) -> Result<atom_syndication::Entry> {
    use atom_syndication::*;

    let categories = labels.iter()
        .map(|name| Category {
            term: name.clone(),
            scheme: None,
            label: None
        })
        .collect::<Vec<_>>();

    Ok(EntryBuilder::default()
        .title(xml_entity_escape(&issue.title))
        .id(xml_entity_escape(&issue.html_url))
        .updated(Utc.timestamp(issue.updated_at, 0))
        .authors(vec![
            Person {
                uri: Some(format!("https://github.com/{}", issue.user_login)),
                name: issue.user_login.clone(),
                email: None
            }
        ])
        .categories(categories)
        .links(vec![LinkBuilder::default()
                        .href(issue.html_url.clone())
                        .build()
                        .expect("Failed to build link")])
        .content(ContentBuilder::default()
                    .content_type(Some(String::from("html")))
                    .value(xml_entity_escape(&issue.body))
                    .build()
                    .expect("Failed to build content"))
        .build()
        .map_err(anyhow::Error::msg)
        .context("Failed to build atom entry")?)
}

async fn issue_to_rss_item(issue: &Issue, labels: &[String]) -> Result<rss::Item> {
    use rss::*;

    let categories = labels.iter()
        .map(|name| CategoryBuilder::default()
             .name(name)
             .build())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err_str| anyhow::anyhow!(err_str))?;

    Ok(ItemBuilder::default()
       .title(xml_entity_escape(&issue.title))
       .link(xml_entity_escape(&issue.html_url))
       .pub_date(Utc.timestamp(issue.updated_at, 0).to_rfc2822())
       .categories(categories)
       .content(xml_entity_escape(&issue.body))
       .build()
       .map_err(anyhow::Error::msg)
       .context("Failed to build RSS item")?)
}

pub async fn run(mut conn: &mut Conn, opts: GenerateOpts) -> Result<()> {
    use atom_syndication::{ FeedBuilder, LinkBuilder };
    use rss::{ ChannelBuilder };

    let (ref owner, ref name) = parse_repo(&opts.repo)?;
    let labels = if opts.labels.is_empty() {
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
    } else { opts.labels };

    let repo_id = repo_id(&mut conn, owner, name).await?;

    let mut state_mask = !0;
    if opts.without_open { state_mask &= !query::issues::IssueState::OPEN.to_integer(); }
    if opts.without_closed { state_mask &= !query::issues::IssueState::CLOSED.to_integer(); }

    for label in labels {
        let feed_directory = opts.out_path.join(path_escape(&label));
        info!("generating {}", feed_directory.display());

        fs::create_dir_all(&feed_directory)?;

        let issues: Vec<Issue> = query_issues_for_label(&mut conn, repo_id, &label, state_mask).await
            .filter_map(|res| async { res.ok() })
            .collect().await;

        let label_url = {
            let mut url = Url::parse("https://github.com")?;
            url.path_segments_mut()
                .unwrap()
                .push(owner).push(name)
                .push("labels").push(&label);
            url.into_string()
        };

        let mut atom_entries = Vec::new();
        let mut rss_items = Vec::new();

        for issue in issues.into_iter() {
            let state_label = query::issues::IssueState::from_integer(issue.state)
                .expect("Inconsistent database, invalid issue state").to_string();
            let labels_of_issue = sqlx::query_as::<_, (String,)>(
                "SELECT labels.name FROM is_labeled
                 JOIN labels ON is_labeled.label=labels.id
                 JOIN issues ON (is_labeled.issue=issues.number AND is_labeled.repo=issues.repo)
                 WHERE is_labeled.repo=? AND is_labeled.issue=?"
            ).bind(repo_id).bind(issue.number)
             .fetch(&mut *conn)
             .filter_map(|row| async { row.ok() })
             .map(|(name,)| name);

            let all_labels = futures::stream::iter(state_label)
                .chain(labels_of_issue)
                .collect::<Vec<_>>()
                .await;

            if opts.atom {
                atom_entries.push(issue_to_atom_entry(&issue, &all_labels[..]).await?);
            }

            if opts.rss {
                rss_items.push(issue_to_rss_item(&issue, &all_labels[..]).await?);
            }
        }

        if opts.atom {
            let mut feed = FeedBuilder::default();
            feed.title(xml_entity_escape(&label));
            feed.id(&label_url);
            feed.updated(Utc::now());
            feed.links(vec![
                LinkBuilder::default()
                    .href(&label_url)
                    .rel("alternate")
                    .build()
                    .map_err(anyhow::Error::msg)?
            ]);
            feed.entries(atom_entries);

            let feed = feed.build().expect("Failed to build Atom feed");
            let feed_path = feed_directory.join("atom.xml");
            let mut out_file = File::create(feed_path)?;
            feed.write_to(&mut out_file)?;
        }

        if opts.rss {
            let mut channel = ChannelBuilder::default();
            channel.title(xml_entity_escape(&label));
            channel.link(&label_url);
            channel.pub_date(Utc::now().to_rfc2822());
            channel.items(rss_items);

            let channel = channel.build().expect("Failed to build RSS channel");
            let channel_path = feed_directory.join("rss.xml");
            let mut out_file = File::create(channel_path)?;
            channel.write_to(&mut out_file)?;
        }
    }

    Ok(())
}
