use graphql_client::{ GraphQLQuery, Response };
use reqwest::Client;

use tracing::{ error, debug };

use crate::{ Conn, query::* };

type URI = String;

#[derive(GraphQLQuery)]
#[graphql(
    // curl https://api.github.com/graphql -H 'Authorization: bearer ...'
    schema_path = "graphql/github.json",
    query_path = "graphql/labels.graphql",
    response_derives = "Debug"
)]
pub struct RepoLabels;

pub async fn update(mut conn: &mut Conn, github_api_token: &str, (ref owner, ref name): (String, String)) -> anyhow::Result<()> {
    let repo = repo_id(&mut conn, owner, name).await?;

    let client = Client::new();

    let mut has_next_page = true;
    let mut last_cursor = None;
    while has_next_page {
        let query = RepoLabels::build_query(repo_labels::Variables {
            owner: owner.to_owned(),
            name: name.to_owned(),
            after: last_cursor.clone()
        });

        let res = graphql::query(&client, github_api_token, query).await?;
        let response: Response<repo_labels::ResponseData> = res.json().await?;

        for error in response.errors.unwrap_or_default() {
            error!("{:?}", error);
        }

        let repository = response.data
            .expect("Missing response data")
            .repository
            .expect("Missing repository");
    
        if repository.labels.is_none() { break }
        let labels = repository.labels.unwrap();
        has_next_page = labels.page_info.has_next_page;
        debug!("has_next_page: {}", has_next_page);
        let labels = labels.edges.unwrap_or_default();

        for label in labels.into_iter().flatten() {
            last_cursor = Some(label.cursor);
            if let Some(label) = label.node {
                debug!("{}: {}", repo, label.name);
                sqlx::query(
                    "INSERT OR IGNORE INTO labels (repo, name) VALUES (?, ?)"
                ).bind(repo).bind(label.name)
                 .execute(&mut conn)
                 .await?;
            }
        }
    }

    Ok(())
}
