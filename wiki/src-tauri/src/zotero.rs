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
//! | `list_all_pdf_items`             | `Result<Vec<ZoteroPdfEntry>, ..>`  | Items + PDF attachment across the entire library     |
//! | `download_attachment`            | `Result<Vec<u8>, String>`          | Raw file bytes for a Zotero attachment (PDF, etc.)   |

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio::time::{sleep, Duration, Instant};

/// Web-API-compatible base URL for the local Zotero library.
///
/// The local HTTP server mirrors the public Web API routing, so every data
/// endpoint (`/items`, `/collections`, …) must be prefixed with a library
/// identifier.  The local user library is always `users/0`; group libraries
/// would use `groups/<id>` but LLM-Wiki only operates on the personal
/// library.  Hitting the bare `/api/collections` returns HTTP 404.
pub const ZOTERO_API: &str = "http://localhost:23119/api/users/0";

/// Root API URL — used only for connectivity probes (`check_status`, `ping`).
/// Data endpoints must use [`ZOTERO_API`] which includes the library prefix.
const ZOTERO_API_ROOT: &str = "http://localhost:23119/api";

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
    /// Authors / editors / contributors as returned by Zotero.
    #[serde(default)]
    pub creators: Vec<ZoteroCreator>,
    /// Free-form date string ("2017", "2017-06-12", "June 2017", …).
    pub date: Option<String>,
    /// `url` field — used as a last-resort link when no DOI exists.
    pub url: Option<String>,
    /// Journal title for journal articles (e.g. "Nature", "JMLR").
    #[serde(rename = "publicationTitle")]
    pub publication_title: Option<String>,
    /// Short journal name (e.g. "Nat. Commun.").
    #[serde(rename = "journalAbbreviation")]
    pub journal_abbreviation: Option<String>,
    /// Conference name for conference papers (e.g. "NeurIPS 2023").
    #[serde(rename = "conferenceName")]
    pub conference_name: Option<String>,
    /// Proceedings title for conference papers (e.g. "Proc. of CVPR").
    #[serde(rename = "proceedingsTitle")]
    pub proceedings_title: Option<String>,
    /// Book title for book chapters.
    #[serde(rename = "bookTitle")]
    pub book_title: Option<String>,
    /// Repository for preprints (e.g. "arXiv", "bioRxiv").
    pub repository: Option<String>,
    /// Publisher (e.g. "MIT Press").
    pub publisher: Option<String>,
}

impl ZoteroItemData {
    /// Pick the most descriptive "venue / publication" field for this item,
    /// trying every Zotero item-type-specific field in priority order.
    ///
    /// Returns the trimmed string, or `None` if every candidate is empty.
    pub fn best_publication(&self) -> Option<String> {
        for candidate in [
            self.publication_title.as_deref(),
            self.conference_name.as_deref(),
            self.proceedings_title.as_deref(),
            self.book_title.as_deref(),
            self.journal_abbreviation.as_deref(),
            self.repository.as_deref(),
            self.publisher.as_deref(),
        ] {
            if let Some(s) = candidate {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
}

/// One creator/author entry returned by the Zotero API.
///
/// Zotero stores either `firstName + lastName` (people) or `name`
/// (institutions, single-field aliases like "OpenAI") — both shapes are
/// allowed in the same `creators` array.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ZoteroCreator {
    #[serde(rename = "creatorType")]
    pub creator_type: Option<String>,
    #[serde(rename = "firstName")]
    pub first_name: Option<String>,
    #[serde(rename = "lastName")]
    pub last_name: Option<String>,
    /// Single-field name (institutions, group authors).
    pub name: Option<String>,
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
    /// The Zotero collection name (lower-case kebab-case) this item belongs to,
    /// or `None` if the item is in the Unclassified collection or has no
    /// collection.  Consumed by the importer as a classification override so
    /// Gemini is skipped for items with a known category.
    pub collection_name: Option<String>,
}

/// Result returned by [`sync_zotero_structure`].
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncStructureResult {
    /// Category folder names created under `content/papers/` (Zotero had the
    /// collection but no folder existed in the wiki).
    pub folders_created: Vec<String>,
    /// Zotero collection names created because the wiki had the folder but
    /// Zotero did not have the corresponding collection.
    pub collections_created: Vec<String>,
    /// Non-fatal errors encountered during the sync (sync continues past them).
    pub errors: Vec<String>,
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
    /// Zotero serialises this as `false` for top-level collections and as
    /// the parent collection's key string otherwise.  `serde_json::Value`
    /// lets us tolerate both without a custom deserialiser.
    #[serde(rename = "parentCollection", default)]
    #[allow(dead_code)]
    parent_collection: serde_json::Value,
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
/// Probes the API *root* (not a data endpoint) — Zotero returns 200 for
/// `/api/` as soon as it is running and the "Allow other applications…"
/// preference is enabled.
///
/// Used by the pending-sync poller to gate retry attempts.
pub(crate) async fn ping(client: &Client) -> bool {
    client
        .get(format!("{ZOTERO_API_ROOT}/"))
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

/// Fetch all Zotero collections and return a `key → full nested path` map.
///
/// The path is built by walking each collection's `parentCollection` chain
/// up to the root and joining the names with `/`.  For Zotero structure
/// `Computer Vision > 01_Generative_Models > Autoencoders` this returns
/// `"Computer Vision/01_Generative_Models/Autoencoders"`.
///
/// Returns an empty map on any network or parse error so callers can
/// degrade gracefully (items will have `collection_name = None`).
pub(crate) async fn fetch_collection_paths_map(client: &Client) -> HashMap<String, String> {
    let resp: serde_json::Value = match client
        .get(format!("{ZOTERO_API}/collections"))
        .send()
        .await
    {
        Ok(r) => match r.error_for_status() {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(v) => v,
                Err(_) => return HashMap::new(),
            },
            Err(_) => return HashMap::new(),
        },
        Err(_) => return HashMap::new(),
    };

    // First pass: collect `(key, name, parent_key)` for every collection.
    let mut nodes: HashMap<String, (String, Option<String>)> = HashMap::new();
    if let Some(arr) = resp.as_array() {
        for col in arr {
            let key = match col["key"].as_str() {
                Some(k) => k.to_string(),
                None => continue,
            };
            let name = match col["data"]["name"].as_str() {
                Some(n) => n.to_string(),
                None => continue,
            };
            // parentCollection is either `false` (top-level) or a key string.
            let parent = col["data"]["parentCollection"].as_str().map(str::to_string);
            nodes.insert(key, (name, parent));
        }
    }

    build_collection_paths(&nodes)
}

/// Pure helper for `fetch_collection_paths_map` — easy to unit-test.
///
/// Walks each entry's parent chain (guarding against cycles) and returns a
/// `key → "Parent/Child/Grandchild"` map.  Each entry is `(name, parent_key)`.
pub(crate) fn build_collection_paths(
    nodes: &HashMap<String, (String, Option<String>)>,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::with_capacity(nodes.len());

    for key in nodes.keys() {
        let mut chain: Vec<&str> = Vec::new();
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut cursor: Option<&str> = Some(key.as_str());
        while let Some(k) = cursor {
            if !visited.insert(k) {
                // Cycle — bail out and keep what we have.
                break;
            }
            let Some((name, parent)) = nodes.get(k) else {
                break;
            };
            chain.push(name.as_str());
            cursor = parent.as_deref();
        }
        chain.reverse();
        out.insert(key.clone(), chain.join("/"));
    }

    out
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
    match client.get(format!("{ZOTERO_API_ROOT}/")).send().await {
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

/// Metadata the organiser uses to locate a Zotero item (step 4).
pub(crate) struct OrganizerPaperHints {
    pub zotero_key: Option<String>,
    pub doi: Option<String>,
    pub title: String,
}

/// Resolve a Zotero library item for the organiser pipeline.
///
/// Lookup order:
///   1. `zotero_key` (injected by the Zotero-driven PDF importer)
///   2. DOI
///   3. title (also used as a fallback when DOI lookup fails)
///
/// Returns `None` when every attempt fails or Zotero is unreachable.
pub(crate) async fn lookup_item_for_organizer(
    hints: &OrganizerPaperHints,
) -> Option<(ZoteroItem, String)> {
    if let Some(zk) = hints.zotero_key.as_ref().filter(|k| !k.is_empty()) {
        if let Ok(item) = get_item_by_key(zk.clone()).await {
            return Some((item, format!("zotero_key {zk}")));
        }
    }

    if let Some(doi) = hints.doi.as_ref().filter(|d| !d.is_empty()) {
        match get_item_by_doi(doi.clone()).await {
            Ok(item) => return Some((item, format!("DOI {doi}"))),
            Err(_) if !hints.title.is_empty() => {
                if let Ok(item) = get_item_by_title(hints.title.clone()).await {
                    return Some((item, format!("title fallback (DOI {doi} not found)")));
                }
            }
            _ => {}
        }
    } else if !hints.title.is_empty() {
        if let Ok(item) = get_item_by_title(hints.title.clone()).await {
            return Some((item, "title (no DOI in frontmatter)".to_string()));
        }
    }

    None
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

    // Resolve the queried collection's full nested path so items returned
    // here pick up the same parent chain as items returned by the
    // entire-library listing.  Falls back to the leaf name if anything
    // unexpected happens.
    let paths_map = fetch_collection_paths_map(&client).await;
    let full_path = paths_map
        .get(&col_key)
        .cloned()
        .unwrap_or_else(|| name.to_string());

    Ok(resolve_pdf_entries(&client, top_items, &HashMap::new(), Some(&full_path)).await)
}

/// List every top-level item in the **entire** user library together with its
/// first PDF attachment.  Used by the "import existing Zotero" flow that
/// re-imports a library that pre-dates LLM-Wiki — no collection filter.
///
/// Items without a PDF child are silently skipped.  Each entry's
/// `collection_name` is populated from Zotero's collection data so the
/// importer can place items in the correct folder without a Gemini call.
#[tauri::command]
pub async fn list_all_pdf_items() -> Result<Vec<ZoteroPdfEntry>, String> {
    let client = build_client();

    // Fetch all collections upfront so we can resolve each item's full
    // nested collection path in O(1) without extra HTTP requests per item.
    let collections_map = fetch_collection_paths_map(&client).await;

    let top_items: Vec<serde_json::Value> = client
        .get(format!("{ZOTERO_API}/items/top"))
        // `limit=100` is the Zotero per-request maximum; if the library is
        // larger we will need pagination, but 100 covers most personal
        // libraries and keeps the first-render latency bounded.
        .query(&[("limit", "100")])
        .send()
        .await
        .map_err(|e| format!("Zotero unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Zotero API error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    Ok(resolve_pdf_entries(&client, top_items, &collections_map, None).await)
}

/// Resolve a list of top-level item JSON blobs into [`ZoteroPdfEntry`] values.
/// Items whose first attachment is not a PDF are dropped.  Sorted by title.
///
/// `collections_map` maps Zotero collection keys to their names — used to
/// populate `ZoteroPdfEntry::collection_name` for each item.
///
/// When `override_collection_name` is `Some(name)`, that name is used for
/// **all** items regardless of their own `data.collections` field.  This is
/// the correct behaviour for [`list_collection_pdf_items`] where all returned
/// items are definitively members of the queried collection.
async fn resolve_pdf_entries(
    client: &Client,
    top_items: Vec<serde_json::Value>,
    collections_map: &HashMap<String, String>,
    override_collection_name: Option<&str>,
) -> Vec<ZoteroPdfEntry> {
    let mut out: Vec<ZoteroPdfEntry> = Vec::with_capacity(top_items.len());

    for item in top_items {
        let Some(item_key) = item["key"].as_str().map(str::to_string) else {
            continue;
        };
        let title = item["data"]["title"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| item_key.clone());

        // Determine collection name: use override when provided (items from a
        // specific collection endpoint), or look up the item's first collection
        // key in the map (full-library listing).
        let collection_name: Option<String> = override_collection_name
            .map(|n| n.to_string())
            .or_else(|| {
                item["data"]["collections"]
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|v| v.as_str())
                    .and_then(|k| collections_map.get(k))
                    .cloned()
            });

        if let Ok(Some(pdf)) = first_pdf_attachment(client, &item_key).await {
            out.push(ZoteroPdfEntry {
                item_key,
                attachment_key: pdf.0,
                title,
                filename: pdf.1,
                collection_name,
            });
        }
    }

    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Recursively gather every category path under `papers_dir`, encoded as a
/// `/`-separated string relative to `papers_dir`.
///
/// Skips hidden directories (`.staging`, `.git`, …) and the top-level
/// `unclassified` folder.  Returns an empty set when the directory does not
/// exist so callers can call this unconditionally.
pub(crate) fn collect_wiki_paths(papers_dir: &Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if !papers_dir.is_dir() {
        return out;
    }
    for entry in walkdir::WalkDir::new(papers_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        let rel = match entry.path().strip_prefix(papers_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let s = rel.to_string_lossy().replace('\\', "/");
        if s.is_empty() {
            continue;
        }
        let top = s.split('/').next().unwrap_or(&s);
        if top.starts_with('.') || top.eq_ignore_ascii_case("unclassified") {
            continue;
        }
        out.insert(s);
    }
    out
}

/// Mirror Zotero's collection hierarchy into `content/papers/`.
///
/// Walks the **full nested path** of every Zotero collection (e.g.
/// `"Computer Vision/01_Generative_Models/Autoencoders"`) and creates the
/// matching nested folder when it is missing from the wiki.
///
/// One-way only: pushing wiki folders back into Zotero is intentionally
/// skipped because creating nested Zotero collections requires the parent
/// collection's key and a separate write per level — the previous best-effort
/// flat POST produced spurious "오류 N" reports for items that actually
/// existed.  Zotero is treated as the source of truth here.
///
/// Best-effort — per-folder I/O failures are collected and reported but do
/// not abort the run.
#[tauri::command]
pub async fn sync_zotero_structure(content_root: String) -> Result<SyncStructureResult, String> {
    let client = build_client();
    let mut result = SyncStructureResult {
        folders_created: vec![],
        collections_created: vec![],
        errors: vec![],
    };

    // ── Build Zotero nested-path set (lowercase-Unclassified excluded) ────
    let paths_map = fetch_collection_paths_map(&client).await;
    if paths_map.is_empty() {
        return Err(
            "Zotero에서 컬렉션 목록을 가져올 수 없습니다. Zotero가 실행 중인지, \
             그리고 \"Allow other applications…\" 설정이 켜져 있는지 확인하세요."
                .into(),
        );
    }
    let zotero_paths: std::collections::HashSet<String> = paths_map
        .values()
        .filter(|p| {
            // Drop a path whose *top-level* segment is "Unclassified" — those
            // items belong to the Gemini-classification flow, not a literal
            // folder.  Nested paths that *contain* the word are preserved.
            let top = p.split('/').next().unwrap_or(p.as_str());
            !top.eq_ignore_ascii_case("unclassified")
        })
        .cloned()
        .collect();

    // ── Walk the wiki folder tree recursively ─────────────────────────────
    let papers_dir = std::path::PathBuf::from(&content_root).join("papers");
    let wiki_paths = collect_wiki_paths(&papers_dir);

    // ── Zotero → wiki: create missing nested folders ──────────────────────
    let mut to_create: Vec<&String> = zotero_paths
        .iter()
        .filter(|p| !wiki_paths.contains(*p))
        .collect();
    // Shorter paths first so a parent exists before any grandchild attempts
    // to populate it (`create_dir_all` makes this redundant for correctness,
    // but it keeps the reporting order intuitive).
    to_create.sort_by_key(|p| (p.matches('/').count(), p.to_string()));

    for path in to_create {
        let dir = papers_dir.join(path);
        match std::fs::create_dir_all(&dir) {
            Ok(()) => result.folders_created.push(path.clone()),
            Err(e) => result
                .errors
                .push(format!("Cannot create folder \"{path}\": {e}")),
        }
    }

    // ── Wiki → Zotero: intentionally skipped ──────────────────────────────
    //
    // The previous bidirectional branch POSTed every top-level wiki folder to
    // `/collections` whenever a case-insensitive name lookup missed.  For
    // nested layouts (e.g. `Computer Vision/01_Generative_Models/Autoencoders`)
    // that produced "오류 N" reports because:
    //   - the local API rejects writes when the user has not allowed them,
    //   - and even when allowed, a flat POST cannot recreate a nested
    //     Zotero hierarchy without the parent collection's key.
    // Zotero is the source of truth for the hierarchy; we mirror it into
    // the wiki, not the other way around.

    Ok(result)
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
                "DOI": "10.1234/example",
                "publicationTitle": "Nature",
                "url": "https://example.com/paper"
            }
        }"#;
        let item: ZoteroItem = serde_json::from_str(raw).unwrap();
        assert_eq!(item.key, "ABCD1234");
        assert_eq!(item.data.doi.as_deref(), Some("10.1234/example"));
        assert_eq!(item.data.best_publication().as_deref(), Some("Nature"));
        assert_eq!(item.data.url.as_deref(), Some("https://example.com/paper"));
    }

    #[test]
    fn best_publication_falls_through_to_conference_then_book() {
        let raw = r#"{
            "publicationTitle": "",
            "conferenceName": "  CVPR 2023  ",
            "bookTitle": "Should Be Skipped"
        }"#;
        let data: ZoteroItemData = serde_json::from_str(raw).unwrap();
        assert_eq!(data.best_publication().as_deref(), Some("CVPR 2023"));
    }

    #[test]
    fn best_publication_returns_none_when_all_empty() {
        let raw = r#"{"publicationTitle": "  "}"#;
        let data: ZoteroItemData = serde_json::from_str(raw).unwrap();
        assert!(data.best_publication().is_none());
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

    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn collect_wiki_paths_includes_nested_dirs() {
        let dir = TempDir::new().unwrap();
        let papers = dir.path().join("papers");
        let deep = papers
            .join("Computer Vision")
            .join("01_Generative_Models")
            .join("Autoencoders");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir_all(papers.join("NLP")).unwrap();
        fs::create_dir_all(papers.join("unclassified")).unwrap();
        fs::create_dir_all(papers.join(".staging")).unwrap();

        let paths = collect_wiki_paths(&papers);
        assert!(paths.contains("Computer Vision"));
        assert!(paths.contains("Computer Vision/01_Generative_Models"));
        assert!(paths.contains("Computer Vision/01_Generative_Models/Autoencoders"));
        assert!(paths.contains("NLP"));
        assert!(
            !paths.iter().any(|p| p.starts_with("unclassified")),
            "unclassified top-level must be excluded"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with(".staging")),
            "hidden folders must be excluded"
        );
    }

    #[test]
    fn collect_wiki_paths_handles_missing_directory() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(collect_wiki_paths(&missing).is_empty());
    }

    /// Flat collections (no parents) map to their own leaf name.
    #[test]
    fn build_collection_paths_handles_flat_collections() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "K1".to_string(),
            ("Computer Vision".to_string(), None),
        );
        nodes.insert("K2".to_string(), ("NLP".to_string(), None));

        let paths = build_collection_paths(&nodes);
        assert_eq!(paths.get("K1").map(String::as_str), Some("Computer Vision"));
        assert_eq!(paths.get("K2").map(String::as_str), Some("NLP"));
    }

    /// Nested chains are joined with `/`, parent → child → grandchild.
    #[test]
    fn build_collection_paths_walks_parent_chain() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "K1".to_string(),
            ("Computer Vision".to_string(), None),
        );
        nodes.insert(
            "K2".to_string(),
            ("01_Generative_Models".to_string(), Some("K1".to_string())),
        );
        nodes.insert(
            "K3".to_string(),
            ("Autoencoders".to_string(), Some("K2".to_string())),
        );

        let paths = build_collection_paths(&nodes);
        assert_eq!(
            paths.get("K3").map(String::as_str),
            Some("Computer Vision/01_Generative_Models/Autoencoders"),
        );
        assert_eq!(
            paths.get("K2").map(String::as_str),
            Some("Computer Vision/01_Generative_Models"),
        );
    }

    /// A pathological self-loop must not hang the resolver.
    #[test]
    fn build_collection_paths_breaks_cycles() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "A".to_string(),
            ("Loop".to_string(), Some("A".to_string())),
        );
        let paths = build_collection_paths(&nodes);
        assert_eq!(paths.get("A").map(String::as_str), Some("Loop"));
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
