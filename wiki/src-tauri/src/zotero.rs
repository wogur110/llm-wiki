//! Zotero local API integration.
//!
//! Communicates with the Zotero connector server at `http://localhost:23119/api`
//! (no auth required — local connections only).
//!
//! # Tauri commands exposed
//! | Command                  | Returns                      | Description                              |
//! |--------------------------|------------------------------|------------------------------------------|
//! | `check_status`           | `ZoteroStatus`               | Connectivity probe                       |
//! | `get_item_by_doi`        | `Result<ZoteroItem, String>` | Fetch the item matching a DOI            |
//! | `get_current_collection` | `Result<String, String>`     | Name of the item's first collection      |
//! | `update_collection`      | `Result<(), String>`         | Move item to a named collection          |
//! | `wait_for_zotmoov`       | `Result<(), String>`         | Block until PDF appears at expected path |

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::time::{sleep, Duration, Instant};

pub const ZOTERO_API: &str = "http://localhost:23119/api";
pub const POLL_INTERVAL_SECS: u64 = 30;

/// Hard timeout for a single Zotero HTTP request (connect + read).
const REQUEST_TIMEOUT_SECS: u64 = 5;

// ── Public types ──────────────────────────────────────────────────────────────

/// Connectivity state of the local Zotero server.
///
/// Serialised for the JS frontend as:
/// * `{"status":"Connected"}`
/// * `{"status":"Disconnected"}`
/// * `{"status":"Error","error":"<reason>"}`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "error")]
pub enum ZoteroStatus {
    /// HTTP 2xx received from the Zotero connector server.
    Connected,
    /// Connection refused or timed out — Zotero is likely not running.
    Disconnected,
    /// Server responded with a non-2xx status or an unexpected error.
    Error(String),
}

/// A Zotero library item as returned by the local connector API.
#[derive(Debug, Serialize, Deserialize)]
pub struct ZoteroItem {
    pub key: String,
    pub data: ZoteroItemData,
}

/// Metadata fields on a Zotero item that LLM-Wiki cares about.
#[derive(Debug, Serialize, Deserialize)]
pub struct ZoteroItemData {
    pub title: Option<String>,
    #[serde(rename = "abstractNote")]
    pub abstract_note: Option<String>,
    /// Collection *keys* (not names) the item belongs to.
    #[serde(default)]
    pub collections: Vec<String>,
    /// DOI string, e.g. `"10.1145/3442188.3445922"`.
    #[serde(rename = "DOI")]
    pub doi: Option<String>,
}

// ── Internal wire types ───────────────────────────────────────────────────────

/// Collection object returned by `GET /collections/{key}`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ZoteroCollection {
    key: String,
    data: ZoteroCollectionData,
}

#[derive(Debug, Deserialize)]
struct ZoteroCollectionData {
    name: String,
}

// ── Client helper ─────────────────────────────────────────────────────────────

fn build_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .expect("failed to build reqwest client")
}

// ── Internal helpers (pub(crate)) ─────────────────────────────────────────────

/// Returns `true` if the Zotero local server answers with HTTP 2xx.
///
/// Used by the pending-sync poller to gate retry attempts.
pub(crate) async fn ping(client: &Client) -> bool {
    client
        .get(format!("{ZOTERO_API}/"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Move `item_key` so it belongs only to `collection_key`.
///
/// Used by the organiser transaction pipeline (step 4).
pub(crate) async fn set_item_collection(
    client: &Client,
    item_key: &str,
    collection_key: &str,
) -> Result<(), anyhow::Error> {
    let url = format!("{ZOTERO_API}/items/{item_key}");
    let body = serde_json::json!({ "collections": [collection_key] });
    client
        .patch(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

/// Find a collection by *name*, or create it if it does not exist.
///
/// Returns the Zotero collection key (not the name).
/// The name must be lower-case kebab-case per the CLAUDE.md spec.
pub(crate) async fn ensure_collection(
    client: &Client,
    name: &str,
) -> Result<String, anyhow::Error> {
    let url = format!("{ZOTERO_API}/collections");

    // Fetch all collections and look for an exact name match.
    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if let Some(arr) = resp.as_array() {
        for col in arr {
            if col["data"]["name"].as_str() == Some(name) {
                if let Some(key) = col["key"].as_str() {
                    return Ok(key.to_string());
                }
            }
        }
    }

    // Collection not found — create it.
    let created: serde_json::Value = client
        .post(&url)
        .json(&serde_json::json!([{ "name": name }]))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let key = created["success"]["0"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no key in create-collection response"))?
        .to_string();

    Ok(key)
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Probe the local Zotero server and return its connectivity status.
///
/// This command **never** returns an error to JS — all outcomes are encoded
/// in the `ZoteroStatus` variants so the frontend can display appropriate UI
/// without a `try/catch`.
#[tauri::command]
pub async fn check_status() -> ZoteroStatus {
    let client = build_client();
    match client.get(format!("{ZOTERO_API}/")).send().await {
        // Connection refused or hard timeout → Zotero is not running.
        Err(e) if e.is_connect() || e.is_timeout() => ZoteroStatus::Disconnected,
        // Other transport/TLS errors.
        Err(e) => ZoteroStatus::Error(e.to_string()),
        Ok(resp) => {
            if resp.status().is_success() {
                ZoteroStatus::Connected
            } else {
                ZoteroStatus::Error(format!("HTTP {}", resp.status()))
            }
        }
    }
}

/// Look up a Zotero library item by its DOI.
///
/// Issues `GET /items?q={doi}` and returns the first result whose `data.DOI`
/// field matches the requested DOI (case-insensitive comparison).
///
/// # Errors
/// * Zotero is unreachable
/// * No item with that DOI is found in the library
#[tauri::command]
pub async fn get_item_by_doi(doi: String) -> Result<ZoteroItem, String> {
    let client = build_client();

    let items: Vec<ZoteroItem> = client
        .get(format!("{ZOTERO_API}/items"))
        .query(&[("q", &doi)])
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Zotero API error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    items
        .into_iter()
        .find(|item| {
            item.data
                .doi
                .as_deref()
                .map(|d| d.eq_ignore_ascii_case(&doi))
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("No Zotero item found for DOI: {doi}"))
}

/// Return the *name* of the first collection the item belongs to.
///
/// The name is the canonical lower-case kebab-case category string shared
/// by LLM-Wiki content folders and ZotMoov target directories.
///
/// # Errors
/// * Zotero is unreachable
/// * The item does not exist or has no collection
/// * The collection key cannot be resolved to a name
#[tauri::command]
pub async fn get_current_collection(item_key: String) -> Result<String, String> {
    let client = build_client();

    // Step 1: fetch the item to get its collection key(s).
    let item: ZoteroItem = client
        .get(format!("{ZOTERO_API}/items/{item_key}"))
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Zotero API error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    let col_key = item
        .data
        .collections
        .into_iter()
        .next()
        .ok_or_else(|| format!("Item {item_key} belongs to no collection"))?;

    // Step 2: resolve collection key → name.
    let col: ZoteroCollection = client
        .get(format!("{ZOTERO_API}/collections/{col_key}"))
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable (collections): {e}"))?
        .error_for_status()
        .map_err(|e| format!("Collection lookup error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Collection JSON decode error: {e}"))?;

    Ok(col.data.name)
}

/// Move a Zotero item to the named collection, creating the collection if needed.
///
/// `collection` must be lower-case kebab-case — the canonical name shared
/// with LLM-Wiki folders and ZotMoov target directories.
///
/// # Errors
/// * Zotero is unreachable
/// * Collection lookup/creation or the PATCH request fails
#[tauri::command]
pub async fn update_collection(
    item_key: String,
    collection: String,
) -> Result<(), String> {
    let client = build_client();

    let col_key = ensure_collection(&client, &collection)
        .await
        .map_err(|e| format!("Could not find/create collection \"{collection}\": {e}"))?;

    set_item_collection(&client, &item_key, &col_key)
        .await
        .map_err(|e| format!("Could not update item collection: {e}"))?;

    Ok(())
}

/// Poll the filesystem until `expected_path` exists or the timeout elapses.
///
/// ZotMoov moves the linked PDF a short time after Zotero's collection field
/// is updated (step 5 of the organiser transaction).  Smart polling starts at
/// 100 ms intervals and backs off to 500 ms after ~2 s to reduce CPU churn
/// while remaining responsive for fast moves.
///
/// # Errors
/// Returns `Err(...)` describing the timeout if the file does not appear
/// within `timeout_secs` seconds.
#[tauri::command]
pub async fn wait_for_zotmoov(
    expected_path: String,
    timeout_secs: u64,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut interval_ms: u64 = 100;

    loop {
        if Path::new(&expected_path).exists() {
            return Ok(());
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "Timeout after {timeout_secs}s: \"{expected_path}\" did not appear"
            ));
        }

        // Sleep at most until the deadline to avoid overshooting.
        let sleep_dur = Duration::from_millis(interval_ms)
            .min(deadline.saturating_duration_since(now));
        sleep(sleep_dur).await;

        // Back off: 100 → 200 → 400 → 500 ms (cap at 500 ms).
        if interval_ms < 500 {
            interval_ms = (interval_ms * 2).min(500);
        }
    }
}
