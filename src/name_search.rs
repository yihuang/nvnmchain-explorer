//! The registry name index, when an operator runs one.
//!
//! The contract answers a whole name and nothing else, so half a name matches only here:
//! nvnmchain-anchoring, on the route the Cosmos module served the search on. A courtesy,
//! never a dependency -- without it the box still finds a whole name, from the contract.

use std::time::Duration;

use serde::Deserialize;

/// A keystroke must not wait on another service.
const TIMEOUT: Duration = Duration::from_secs(2);

/// `RegistryNameMatchMode.REGISTRY_NAME_MATCH_MODE_PREFIX`.
const PREFIX: &str = "2";

const SEARCH_PATH: &str = "/NVNM-Chain/nvnmchain/anchoring/v1/registries/search";

/// A registry the index matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub id: u64,
    pub name: String,
}

/// The service's answer. Proto JSON, so `id` is a string.
#[derive(Deserialize)]
struct Answer {
    registries: Option<Vec<Row>>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    name: String,
}

/// Registries whose name starts with `q`, in id order, at most `limit`.
///
/// Empty for anything that goes wrong: the box is better without a row than broken by a
/// service that is down, slow, or answering something else.
pub async fn prefix(client: &reqwest::Client, base: &str, q: &str, limit: usize) -> Vec<Named> {
    match ask(client, base, q, limit).await {
        Ok(named) => named,
        Err(e) => {
            tracing::debug!("name index: {e}");
            Vec::new()
        }
    }
}

async fn ask(
    client: &reqwest::Client,
    base: &str,
    q: &str,
    limit: usize,
) -> reqwest::Result<Vec<Named>> {
    let answer: Answer = client
        .get(format!("{}{SEARCH_PATH}", base.trim_end_matches('/')))
        .query(&[
            ("name", q),
            ("mode", PREFIX),
            ("pagination.limit", &limit.to_string()),
        ])
        .timeout(TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(answer
        .registries
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            Some(Named {
                id: row.id.parse().ok()?,
                name: row.name,
            })
        })
        .collect())
}
