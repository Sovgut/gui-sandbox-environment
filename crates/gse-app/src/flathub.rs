//! Searching Flathub for apps to put into a set, through its public API.

use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::Deserialize;

const SEARCH: &str = "https://flathub.org/api/v2/search";

/// The most results a search shows.
const SHOWN: usize = 20;

/// An app Flathub found.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Found {
    pub name: String,
    pub app_id: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Deserialize)]
struct Reply {
    hits: Vec<Found>,
}

/// The apps Flathub finds for `query`, best first.
pub fn search(query: &str) -> Result<Vec<Found>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let reply: Reply = agent
        .post(SEARCH)
        .send_json(search_body(query))
        .context("asking Flathub")?
        .into_json()
        .context("reading Flathub's answer")?;
    Ok(reply.hits.into_iter().take(SHOWN).collect())
}

/// The body of a search for `query`.
fn search_body(query: &str) -> ureq::serde_json::Value {
    ureq::serde_json::json!({ "query": query, "filters": [] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_read_with_their_ids() {
        let reply: Reply = ureq::serde_json::from_str(
            r#"{"hits":[{"name":"Lutris","summary":"Games","app_id":"net.lutris.Lutris","icon":"x"}],"total":1}"#,
        )
        .unwrap();
        assert_eq!(
            reply.hits,
            [Found {
                name: "Lutris".into(),
                app_id: "net.lutris.Lutris".into(),
                summary: "Games".into(),
            }]
        );
    }
}
