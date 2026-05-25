//! Zotero local API integration.
//!
//! Communicates with the Zotero connector server at `http://localhost:23119/api`
//! (no auth required — local connections only).
//!
//! # Tauri commands exposed
//! | Command                          | Returns                            | Description                                          |
//! |----------------------------------|------------------------------------|------------------------------------------------------|
//! | `check_status`                   | `ZoteroStatus`                     | Connectivity probe                                   |
//! | `get_item_by_doi`                | `Result<ZoteroItem, String>`       | Fetch the item matching a DOI                        |
//! | `get_item_by_title`              | `Result<ZoteroItem, String>`       | DOI-less fallback lookup                             |
//! | `get_current_collection`         | `Result<String, String>`           | Name of the item's first collection                  |
//! | `update_collection`              | `Result<(), String>`               | Move item to a named collection                      |
//! | `wait_for_zotmoov`               | `Result<(), String>`               | Block until PDF appears at expected path             |
//! | `list_collection_pdf_items`      | `Result<Vec<ZoteroPdfEntry>, ..>`  | Items + PDF attachment in a named collection         |
//! | `download_attachment`            | `Result<Vec<u8>, String>`          | Raw file bytes for a Zotero attachment (PDF, etc.)   |

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

/// A top-level Zotero item paired with its first PDF attachment, ready to be
/// imported.  Returned by [`list_collection_pdf_items`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ZoteroPdfEntry {
    /// Top-level item key (used later in step 4 of the organiser pipeline).
    pub item_key: String,
    /// PDF attachment key — pass to [`download_attachment`].
    pub attachment_key: String,
    /// Display title for progress UI.
    pub title: String,
    /// Original filename of the PDF attachment, if Zotero exposes one.  Used
    /// when picking a markdown filename so wikilinks match the source paper.
    pub filename: Option<String>,
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
    use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

    // Zotero 7's local API blocks requests whose User-Agent starts with
    // "Mozilla/" unless the `Zotero-Allowed-Request` header is present.
    // reqwest's default UA does not match that pattern, but we set both
    // headers defensively so the app keeps working even if a future
    // dependency upgrade changes the UA shape.
    //
    // Reference: https://groups.google.com/g/zotero-dev/c/5KM1QVUOeck
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("LLM-Wiki/0.1 (+tauri)"));
    headers.insert(
        "Zotero-Allowed-Request",
        HeaderValue::from_static("1"),
    );

    Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .default_headers(headers)
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
            let status = resp.status();
            if status.is_success() {
                ZoteroStatus::Connected
            } else if status.as_u16() == 403 {
                // Zotero is running but the local API is locked. Steer the
                // user to the exact preference they need to flip.
                ZoteroStatus::Error(
                    "Zotero가 로컬 API 접근을 차단했습니다. \
                     Zotero → Settings → Advanced → General에서 \
                     \"Allow other applications on this computer to communicate with Zotero\"를 \
                     체크하고 Zotero를 재시작하세요."
                        .to_string(),
                )
            } else {
                ZoteroStatus::Error(format!("HTTP {status}"))
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

/// Look up a Zotero library item by its primary key.
///
/// Used by the Zotero-driven PDF importer so the organiser pipeline can keep
/// working without a DOI or title heuristic (the item key is already known
/// from listing the unclassified collection).
///
/// # Errors
/// * Zotero unreachable
/// * No item with that key exists
#[tauri::command]
pub async fn get_item_by_key(item_key: String) -> Result<ZoteroItem, String> {
    let client = build_client();
    client
        .get(format!("{ZOTERO_API}/items/{item_key}"))
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Zotero API error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))
}

/// Look up a Zotero library item by *title*.  Used as a fallback when no DOI
/// is available (e.g., Gemini failed to extract one from the PDF).
///
/// Matches case-insensitively.  Whitespace inside the title is normalised so
/// that double-spaces and line wraps do not prevent a match.
///
/// # Errors
/// * Zotero unreachable
/// * No item whose title matches the request
#[tauri::command]
pub async fn get_item_by_title(title: String) -> Result<ZoteroItem, String> {
    let client = build_client();
    let normalised = normalise_title(&title);

    let items: Vec<ZoteroItem> = client
        .get(format!("{ZOTERO_API}/items"))
        .query(&[("q", title.as_str()), ("qmode", "titleCreatorYear")])
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
                .title
                .as_deref()
                .map(|t| normalise_title(t) == normalised)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("No Zotero item found for title: {title:?}"))
}

/// Collapse repeated whitespace into single spaces, trim, and lowercase.
fn normalise_title(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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

/// List every top-level item in the named collection together with its first
/// PDF attachment.  Items without a PDF child are silently skipped — they
/// cannot be auto-imported.
///
/// Empty `collection` is rejected up-front to avoid accidentally enumerating
/// the entire library.
///
/// # Errors
/// * Zotero unreachable / 4xx / 5xx
/// * Collection with the given name does not exist
#[tauri::command]
pub async fn list_collection_pdf_items(
    collection: String,
) -> Result<Vec<ZoteroPdfEntry>, String> {
    let name = collection.trim();
    if name.is_empty() {
        return Err("collection name must not be empty".into());
    }

    let client = build_client();

    // ── Find the collection key by name ───────────────────────────────────
    let collections: serde_json::Value = client
        .get(format!("{ZOTERO_API}/collections"))
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Zotero API error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    let col_key = collections
        .as_array()
        .and_then(|arr| {
            arr.iter().find_map(|c| {
                if c["data"]["name"].as_str() == Some(name) {
                    c["key"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| format!("Zotero collection \"{name}\" not found"))?;

    // ── Top-level items in that collection ────────────────────────────────
    let top_items: Vec<serde_json::Value> = client
        .get(format!("{ZOTERO_API}/collections/{col_key}/items/top"))
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable (collection items): {e}"))?
        .error_for_status()
        .map_err(|e| format!("Collection items error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    let mut out: Vec<ZoteroPdfEntry> = Vec::with_capacity(top_items.len());

    for item in top_items {
        let Some(item_key) = item["key"].as_str().map(str::to_string) else {
            continue;
        };
        let title = item["data"]["title"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| item_key.clone());

        if let Ok(Some(pdf)) = first_pdf_attachment(&client, &item_key).await {
            out.push(ZoteroPdfEntry {
                item_key,
                attachment_key: pdf.0,
                title,
                filename: pdf.1,
            });
        }
    }

    // Stable ordering for the UI.
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Ok(out)
}

/// Return `(attachment_key, filename)` for the item's first PDF attachment,
/// or `None` if the item has no PDF.  Best-effort — any HTTP failure on the
/// children endpoint propagates as `Err`.
async fn first_pdf_attachment(
    client: &Client,
    item_key: &str,
) -> Result<Option<(String, Option<String>)>, anyhow::Error> {
    let children: Vec<serde_json::Value> = client
        .get(format!("{ZOTERO_API}/items/{item_key}/children"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(children.into_iter().find_map(|c| {
        let is_pdf = c["data"]["contentType"].as_str() == Some("application/pdf");
        if !is_pdf {
            return None;
        }
        let key = c["key"].as_str()?.to_string();
        // `filename` for imported files, `path` for linked files
        // (which may contain "attachments:foo.pdf").
        let filename = c["data"]["filename"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                c["data"]["path"]
                    .as_str()
                    .map(|p| p.trim_start_matches("attachments:").to_string())
                    .and_then(|p| {
                        std::path::PathBuf::from(p)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(str::to_string)
                    })
            });
        Some((key, filename))
    }))
}

/// Download an attachment's raw file bytes (PDFs, EPUBs, …) via the local API.
///
/// The Zotero local API serves both imported and linked attachments through
/// the same `/items/{key}/file` endpoint, so the caller does not need to know
/// the link mode.  Capped at 50 MB to keep memory usage predictable; if a
/// paper is larger, fall back to ingesting it manually.
///
/// # Errors
/// * Zotero unreachable or attachment missing
/// * File exceeds 50 MB
#[tauri::command]
pub async fn download_attachment(attachment_key: String) -> Result<Vec<u8>, String> {
    const MAX_BYTES: u64 = 50 * 1024 * 1024;

    let client = Client::builder()
        // Generous timeout — large papers can take a while on slow disks.
        .timeout(Duration::from_secs(60))
        .default_headers({
            use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
            let mut h = HeaderMap::new();
            h.insert(USER_AGENT, HeaderValue::from_static("LLM-Wiki/0.1 (+tauri)"));
            h.insert("Zotero-Allowed-Request", HeaderValue::from_static("1"));
            h
        })
        .build()
        .map_err(|e| format!("HTTP client build failed: {e}"))?;

    let resp = client
        .get(format!("{ZOTERO_API}/items/{attachment_key}/file"))
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Attachment download error: {e}"))?;

    if let Some(len) = resp.content_length() {
        if len > MAX_BYTES {
            return Err(format!(
                "Attachment is {} MB; LLM-Wiki caps single-PDF imports at 50 MB.",
                len / (1024 * 1024)
            ));
        }
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Could not read attachment bytes: {e}"))?;

    Ok(bytes.to_vec())
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// When nothing is listening on localhost:23119, `check_status` must return
    /// `ZoteroStatus::Disconnected` (not `Error`).
    #[tokio::test]
    async fn test_check_status_when_offline() {
        let status = check_status().await;
        assert!(
            matches!(status, ZoteroStatus::Disconnected),
            "Expected Disconnected when Zotero is not running, got {:?}",
            status
        );
    }

    #[test]
    fn test_zotero_status_serialises_connected() {
        let json = serde_json::to_string(&ZoteroStatus::Connected).unwrap();
        assert!(json.contains("Connected"));
        let back: ZoteroStatus = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZoteroStatus::Connected));
    }

    #[test]
    fn test_zotero_item_deserialises_from_api_shape() {
        let raw = r#"{
            "key": "ABCD1234",
            "data": {
                "title": "Test Paper",
                "abstractNote": "An abstract.",
                "collections": ["COL1"],
                "DOI": "10.1234/example"
            }
        }"#;
        let item: ZoteroItem = serde_json::from_str(raw).unwrap();
        assert_eq!(item.key, "ABCD1234");
        assert_eq!(item.data.doi.as_deref(), Some("10.1234/example"));
    }

    #[tokio::test]
    async fn test_ping_returns_false_when_offline() {
        let client = build_client();
        assert!(!ping(&client).await);
    }

    #[tokio::test]
    async fn test_get_item_by_doi_when_offline() {
        let err = get_item_by_doi("10.1234/offline-test".into())
            .await
            .unwrap_err();
        assert!(err.contains("Zotero unreachable") || err.contains("Zotero API"));
    }

    #[tokio::test]
    async fn test_get_current_collection_when_offline() {
        let err = get_current_collection("ITEMKEY12".into())
            .await
            .unwrap_err();
        assert!(err.contains("Zotero unreachable") || err.contains("Zotero API"));
    }

    #[tokio::test]
    async fn test_update_collection_when_offline() {
        let err = update_collection("ITEMKEY12".into(), "nlp".into())
            .await
            .unwrap_err();
        assert!(err.contains("collection"));
    }

    /// `wait_for_zotmoov` must time out and return `Err` when the target path
    /// never appears.  Uses a 2-second timeout to keep the test suite fast.
    #[tokio::test]
    async fn test_wait_for_zotmoov_timeout() {
        let nonexistent = "/tmp/llm-wiki-test-nonexistent-pdf-99999.pdf";
        let start = std::time::Instant::now();

        let result = wait_for_zotmoov(nonexistent.to_string(), 2).await;

        let elapsed = start.elapsed();
        assert!(result.is_err(), "Expected Err on timeout, got Ok(())");
        assert!(
            elapsed.as_secs() >= 2,
            "Timeout elapsed too fast ({elapsed:?})"
        );
        // Sanity-check the error message contains path info.
        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains(nonexistent),
            "Error message should mention the path: {err_msg}"
        );
    }
}
