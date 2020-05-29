use std::{ env, io, path::PathBuf };
use structopt::StructOpt;
use sqlx::SqlitePool;
use tracing::info;
use tracing_subscriber::{
    fmt, filter,
    layer::SubscriberExt,
    util::SubscriberInitExt
};

use anyhow::{ anyhow, Result, Context };

pub mod query;
pub mod atom;

#[derive(StructOpt)]
#[structopt(name = "github-label-feed")]
struct Opt {
    #[structopt(subcommand)]
    mode: OptMode,
}

#[derive(StructOpt)]
enum OptMode {
    List,
    Sync {
        repo: String,
        #[structopt(long = "github-api-token", env = "GITHUB_TOKEN", hide_env_values = true)]
        github_api_token: String
    },
    Atom {
        repo: String,
        out_path: PathBuf,
        labels: Vec<String>,
        #[structopt(long)]
        only_open: bool
    }
}


pub type Conn = sqlx::SqliteConnection;

async fn init_db(conn: &mut Conn) {
    // Naive init, all data is re-fetch-able, so no support for migrations
    sqlx::query(r#"
        PRAGMA foreign_keys = ON;
        PRAGMA synchronous = OFF;

        CREATE TABLE IF NOT EXISTS repositories(
            id integer PRIMARY KEY,
            owner text, name text,
            UNIQUE (owner, name)
        );

        CREATE TABLE IF NOT EXISTS issues(
            repo integer REFERENCES repositories,
            number integer,
            state integer, title text, body text,
            user_login text,
            html_url text,
            updated_at integer,
            PRIMARY KEY (repo, number)
        );

        CREATE TABLE IF NOT EXISTS labels(
            id integer PRIMARY KEY,
            repo integer REFERENCES repositories,
            name text,
            UNIQUE (repo, name)
        );

        CREATE TABLE IF NOT EXISTS is_labeled(
            repo integer, issue integer,
            label integer RFERENCES labels,
            PRIMARY KEY (issue, label),
            FOREIGN KEY (repo, issue) REFERENCES issues
        );
    "#).execute(conn)
       .await
       .expect("Failed to init database");
}

pub fn parse_repo(combined: &str) -> Result<(String, String)> {
    let mut parts = combined
        .split('/')
        .map(str::trim)
        .map(str::to_owned);

    match (parts.next(), parts.next()) {
        (Some(r), Some(n)) => Ok((r, n)),
        _ => Err(anyhow!("invalid repo format, expected owner/name: '{}'", combined))
    }
}

fn main() -> Result<()> {
    let env_spec = env::var("RUST_LOG")
        .unwrap_or_else(|_| String::from("info"));
    tracing_subscriber::registry()
        .with(fmt::layer()
              .without_time()
              .with_writer(io::stderr))
        .with(filter::EnvFilter::new(env_spec))
        .init();

    let opt = Opt::from_args();

    smol::run(async {
        let pool = SqlitePool::new("sqlite:./issues.sqlite").await?;
        init_db(&mut *pool.acquire().await?).await;

        match opt.mode {
            OptMode::List => {
                let repos = query::list_repositories(&mut *pool.acquire().await?).await?;
                for query::RepositoryInfo { owner, name, label_count, issue_count, .. } in repos {
                    println!("{}/{} ({} labels, {} issues)", owner, name, label_count, issue_count);
                }
                Ok(())
            },
            OptMode::Sync { repo, github_api_token } => {
                info!("sync");
                let repo = parse_repo(&repo)?;
                let mut tx = pool.begin().await?;
                query::labels::update(&mut tx, &github_api_token, repo.clone())
                    .await
                    .context("Failed to update labels")?;
                query::issues::update(&mut tx, &github_api_token, repo)
                    .await
                    .context("Failed to update issues")?;
                tx.commit().await?;
                Ok(())
            },
            OptMode::Atom { repo, out_path, labels, only_open } => {
                let repo = parse_repo(&repo)?;
                atom::generate(&mut *pool.acquire().await?, repo, out_path, labels, only_open).await
                    .context("Failed to generate Atom feed")?;
                Ok(())
            }
        }
    })
}
