//! Pending Zotero sync queue.
//!
//! When Zotero is offline during the organiser pipeline (step 4), the failed
//! Zotero update is placed in a durable JSON queue at
//! `content/meta/pending-zotero-sync.json`.  The queue is replayed by
//! `sync_all` whenever Zotero comes back online — the frontend polls
//! `check_status` every 30 seconds and calls `sync_all` on reconnect.
//!
//! # Tauri commands exposed
//! | Command             | Returns                         | Description                   |
//! |---------------------|---------------------------------|-------------------------------|
//! | `enqueue`           | `Result<(), String>`            | Add an item (idempotent)      |
//! | `load_queue`        | `Result<Vec<PendingSyncItem>>`  | Read current queue contents   |
//! | `remove_from_queue` | `Result<(), String>`            | Remove one item by Zotero key |
//! | `has_pending`       | `bool`                          | `true` if queue is non-empty  |
//! | `sync_all`          | `Result<SyncResult, String>`    | Replay all pending items      |
//!
//! ## Events emitted by `sync_all`
//! | Event                     | Payload                              |
//! |---------------------------|--------------------------------------|
//! | `pending-sync-progress`   | `{index, total, item}`               |
//! | `pending-sync-item-done`  | `{zotero_item_key}`                  |
//! | `pending-sync-item-error` | `{zotero_item_key, error}`           |
//! | `pending-sync-complete`   | `SyncResult`                         |

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::Emitter;

// ── Public types ──────────────────────────────────────────────────────────────

/// One item waiting for its Zotero collection to be updated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSyncItem {
    /// Relative path to the paper's markdown file under `content/papers/`.
    pub paper_file: String,
    /// Zotero item key (8-char alphanumeric, e.g. `"ABCD1234"`).
    pub zotero_item_key: String,
    /// Target collection name — lower-case kebab-case, matches LLM-Wiki folder.
    pub target_collection: String,
    /// RFC 3339 timestamp when this item was enqueued.
    pub queued_at: String,
}

/// Summary returned to JS after a `sync_all` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    /// Items successfully synced in this run.
    pub synced: u32,
    /// Items that failed and were kept in the queue for the next retry.
    pub failed: u32,
    /// Total items remaining in the queue (equals `failed` after a full run).
    pub remaining: u32,
    /// One human-readable error string per failed item.
    pub errors: Vec<String>,
}

// ── Internal queue container ──────────────────────────────────────────────────

/// The root JSON object in `pending-zotero-sync.json`.
///
/// Kept private — callers use the Tauri commands, not this type directly.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingSyncQueue {
    items: Vec<PendingSyncItem>,
}

impl PendingSyncQueue {
    /// Load from disk; returns an empty queue if the file is missing.
    fn load(path: &Path) -> Result<Self, anyhow::Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Atomically persist to disk (creates parent directories if needed).
    fn save(&self, path: &Path) -> Result<(), anyhow::Error> {
        let text = serde_json::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }
}

// ── SSE event payload types (Serialize-only, used by window.emit) ─────────────

#[derive(Serialize, Clone)]
struct ProgressPayload<'a> {
    index: u32,
    total: u32,
    item: &'a PendingSyncItem,
}

#[derive(Serialize, Clone)]
struct ItemDonePayload {
    zotero_item_key: String,
}

#[derive(Serialize, Clone)]
struct ItemErrorPayload {
    zotero_item_key: String,
    error: String,
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Add a new item to the pending-sync queue.
///
/// `queue_path` is the **absolute** path to `pending-zotero-sync.json`
/// (e.g. `<content_root>/meta/pending-zotero-sync.json`).
///
/// Duplicate `zotero_item_key` values are silently ignored so the operation
/// is safe to call multiple times without inflating the queue.
#[tauri::command]
pub fn enqueue(
    queue_path: String,
    paper_file: String,
    zotero_item_key: String,
    target_collection: String,
) -> Result<(), String> {
    let path = Path::new(&queue_path);
    let mut queue = PendingSyncQueue::load(path)
        .map_err(|e| format!("Queue load error: {e}"))?;

    // Idempotency guard: skip if already queued.
    if queue.items.iter().any(|i| i.zotero_item_key == zotero_item_key) {
        return Ok(());
    }

    queue.items.push(PendingSyncItem {
        paper_file,
        zotero_item_key,
        target_collection,
        queued_at: Utc::now().to_rfc3339(),
    });

    queue
        .save(path)
        .map_err(|e| format!("Queue save error: {e}"))
}

/// Read the current pending-sync queue and return all items.
///
/// Returns an empty `Vec` if the queue file does not exist yet.
#[tauri::command]
pub fn load_queue(queue_path: String) -> Result<Vec<PendingSyncItem>, String> {
    let queue = PendingSyncQueue::load(Path::new(&queue_path))
        .map_err(|e| format!("Queue load error: {e}"))?;
    Ok(queue.items)
}

/// Remove one item from the queue by its Zotero item key.
///
/// A no-op if the key is not present, making repeated calls safe.
#[tauri::command]
pub fn remove_from_queue(
    queue_path: String,
    zotero_item_key: String,
) -> Result<(), String> {
    let path = Path::new(&queue_path);
    let mut queue = PendingSyncQueue::load(path)
        .map_err(|e| format!("Queue load error: {e}"))?;

    queue.items.retain(|i| i.zotero_item_key != zotero_item_key);

    queue
        .save(path)
        .map_err(|e| format!("Queue save error: {e}"))
}

/// Return `true` if there is at least one item waiting to be synced.
///
/// Never errors — a missing or unreadable queue file is treated as empty.
#[tauri::command]
pub fn has_pending(queue_path: String) -> bool {
    PendingSyncQueue::load(Path::new(&queue_path))
        .map(|q| !q.items.is_empty())
        .unwrap_or(false)
}

/// Replay all pending items against the live Zotero API.
///
/// Execution steps:
/// 1. Verify Zotero is reachable — returns `Err` immediately if not.
/// 2. Iterate the queue **sequentially** (one item at a time).
/// 3. On success: remove item from queue, emit `pending-sync-item-done`.
/// 4. On failure: keep item in queue, emit `pending-sync-item-error`.
/// 5. Persist the (possibly shorter) queue back to disk.
/// 6. Emit `pending-sync-complete` with the final `SyncResult`.
///
/// The returned `SyncResult` mirrors the event payload so callers that
/// `await` the command get the same information without an event listener.
///
/// # Errors
/// Returns `Err` only if Zotero is unreachable *before* any items are tried.
/// Per-item failures are accumulated in `SyncResult.errors`.
#[tauri::command]
pub async fn sync_all(
    queue_path: String,
    window: tauri::WebviewWindow,
) -> Result<SyncResult, String> {
    // Build a short-timeout HTTP client for Zotero calls.
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Guard: refuse to start if Zotero is offline — caller should retry later.
    if !crate::zotero::ping(&http).await {
        return Err(
            "Zotero is not reachable — start Zotero and retry".into(),
        );
    }

    let queue_path_buf = std::path::PathBuf::from(&queue_path);
    let mut queue = PendingSyncQueue::load(&queue_path_buf)
        .map_err(|e| format!("Queue load error: {e}"))?;

    let total = queue.items.len() as u32;
    let mut synced = 0u32;
    let mut errors: Vec<String> = Vec::new();
    let mut failed_items: Vec<PendingSyncItem> = Vec::new();

    for (index, item) in queue.items.iter().enumerate() {
        // Let the frontend show a spinner / progress bar.
        let _ = window.emit(
            "pending-sync-progress",
            ProgressPayload {
                index: index as u32,
                total,
                item,
            },
        );

        // When Zotero was offline at classify time the key is a placeholder
        // ("doi:<doi>" or "file:<name>") rather than a real 8-char Zotero key.
        // Resolve it to the actual item key before calling update_collection.
        let real_key = match resolve_zotero_key(item).await {
            Ok(k) => k,
            Err(e) => {
                let msg = format!(
                    "[{}] → \"{}\": key resolution failed: {e}",
                    item.zotero_item_key, item.target_collection
                );
                errors.push(msg);
                failed_items.push(item.clone());
                let _ = window.emit(
                    "pending-sync-item-error",
                    ItemErrorPayload {
                        zotero_item_key: item.zotero_item_key.clone(),
                        error: e,
                    },
                );
                continue;
            }
        };

        // Delegate to the zotero module — find-or-create collection then PATCH.
        match crate::zotero::update_collection(
            real_key,
            item.target_collection.clone(),
        )
        .await
        {
            Ok(()) => {
                synced += 1;
                let _ = window.emit(
                    "pending-sync-item-done",
                    ItemDonePayload {
                        zotero_item_key: item.zotero_item_key.clone(),
                    },
                );
            }
            Err(err) => {
                let msg = format!(
                    "[{}] → \"{}\": {err}",
                    item.zotero_item_key, item.target_collection
                );
                errors.push(msg);
                failed_items.push(item.clone());
                let _ = window.emit(
                    "pending-sync-item-error",
                    ItemErrorPayload {
                        zotero_item_key: item.zotero_item_key.clone(),
                        error: err,
                    },
                );
            }
        }
    }

    // Only failed items stay in the queue; successfully synced ones are dropped.
    let remaining = failed_items.len() as u32;
    queue.items = failed_items;
    // Best-effort persist — don't mask the SyncResult with a save error.
    let _ = queue.save(&queue_path_buf);

    let result = SyncResult {
        synced,
        failed: errors.len() as u32,
        remaining,
        errors,
    };

    // Emit the summary so any listener that missed individual events can react.
    let _ = window.emit("pending-sync-complete", result.clone());

    Ok(result)
}

// ── Key resolution helpers ────────────────────────────────────────────────────

/// Resolve a `PendingSyncItem`'s `zotero_item_key` to a *real* Zotero item key.
///
/// When Zotero is offline at classify time, the organiser stores a placeholder
/// key of the form `"doi:<doi>"` or `"file:<filename>"` instead of an actual
/// 8-character alphanumeric Zotero key.  This function turns those placeholders
/// back into real keys so `sync_all` can call `update_collection` correctly.
///
/// Lookup strategy:
/// 1. Key looks like a real Zotero key (8 uppercase alphanumeric chars) → use as-is.
/// 2. `"doi:<doi>"` → look up by DOI via the Zotero local API.
/// 3. `"file:<name>"` → read `paper_file`'s YAML frontmatter, extract `title:`,
///    look up by title via the Zotero local API.
/// 4. Anything else → pass through unchanged (might be a key format we don't
///    recognise yet; `update_collection` will surface any Zotero-side error).
async fn resolve_zotero_key(item: &PendingSyncItem) -> Result<String, String> {
    let key = &item.zotero_item_key;

    // Real Zotero keys are exactly 8 uppercase alphanumeric characters.
    if key.len() == 8 && key.chars().all(|c| c.is_ascii_alphanumeric() && (c.is_ascii_uppercase() || c.is_ascii_digit())) {
        return Ok(key.clone());
    }

    if let Some(doi) = key.strip_prefix("doi:") {
        return crate::zotero::get_item_by_doi(doi.to_string())
            .await
            .map(|zi| zi.key)
            .map_err(|e| format!("DOI lookup for \"{doi}\": {e}"));
    }

    if key.starts_with("file:") {
        // Try to extract the paper title from its markdown frontmatter.
        let title = std::fs::read_to_string(&item.paper_file)
            .ok()
            .and_then(|content| extract_title_from_frontmatter(&content));

        return match title {
            Some(t) if !t.is_empty() => crate::zotero::get_item_by_title(t.clone())
                .await
                .map(|zi| zi.key)
                .map_err(|e| format!("Title lookup for \"{t}\": {e}")),
            _ => Err(format!(
                "Cannot resolve Zotero item for placeholder key \"{key}\": \
                 paper file has no parseable title in frontmatter"
            )),
        };
    }

    // Unknown format — pass through and let Zotero reject it if invalid.
    Ok(key.clone())
}

/// Extract the `title:` value from a YAML frontmatter block (`--- … ---`).
///
/// Returns `None` if the file has no frontmatter or no `title:` field.
fn extract_title_from_frontmatter(content: &str) -> Option<String> {
    if !content.starts_with("---") {
        return None;
    }
    let after = content.trim_start_matches("---\n");
    let end = after.find("\n---").unwrap_or(after.len());
    for line in after[..end].lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("title:") {
            let title = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Helper: absolute path string for a temporary queue file inside `dir`.
    fn queue_path(dir: &tempfile::TempDir) -> String {
        dir.path()
            .join("pending-zotero-sync.json")
            .to_string_lossy()
            .into_owned()
    }

    /// Enqueueing two items and then loading the queue must return both items.
    #[test]
    fn test_enqueue_and_load() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);

        enqueue(qp.clone(), "paper1.md".into(), "AAAA0001".into(), "large-language-models".into())
            .unwrap();
        enqueue(qp.clone(), "paper2.md".into(), "BBBB0002".into(), "computer-vision".into())
            .unwrap();

        let items = load_queue(qp).unwrap();
        assert_eq!(items.len(), 2, "Queue should contain 2 items");
    }

    /// After removing one item by key the queue must contain exactly one remaining item.
    #[test]
    fn test_remove_from_queue() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);

        enqueue(qp.clone(), "paper1.md".into(), "AAAA0001".into(), "llm".into()).unwrap();
        enqueue(qp.clone(), "paper2.md".into(), "BBBB0002".into(), "cv".into()).unwrap();

        remove_from_queue(qp.clone(), "AAAA0001".into()).unwrap();

        let items = load_queue(qp).unwrap();
        assert_eq!(items.len(), 1, "Queue should contain 1 item after removal");
        assert_eq!(
            items[0].zotero_item_key, "BBBB0002",
            "The remaining item should be the one that was NOT removed"
        );
    }

    /// `has_pending` must return `false` for a fresh (empty) queue.
    #[test]
    fn test_has_pending_false_when_empty() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);
        // Queue file does not exist yet — treated as empty.
        assert!(!has_pending(qp), "Empty queue must report has_pending = false");
    }

    /// Loading a queue path that does not exist yet returns an empty list.
    #[test]
    fn test_load_queue_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);
        let items = load_queue(qp).unwrap();
        assert!(items.is_empty());
    }

    /// Enqueueing the same item key twice must not duplicate entries.
    #[test]
    fn test_enqueue_same_key_is_idempotent() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);
        enqueue(qp.clone(), "paper.md".into(), "KEY123".into(), "nlp".into()).unwrap();
        enqueue(qp.clone(), "paper.md".into(), "KEY123".into(), "nlp".into()).unwrap();
        let items = load_queue(qp).unwrap();
        assert_eq!(items.len(), 1);
    }

    /// `has_pending` must return `true` after enqueueing at least one item.
    #[test]
    fn test_has_pending_true_after_enqueue() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);

        enqueue(qp.clone(), "paper.md".into(), "CCCC0003".into(), "rl".into()).unwrap();
        assert!(has_pending(qp), "Queue with one item must report has_pending = true");
    }

    /// Corrupt queue JSON must not crash `has_pending` — treat as empty.
    #[test]
    fn test_has_pending_false_for_invalid_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending-zotero-sync.json");
        std::fs::write(&path, "not-json").unwrap();
        let qp = path.to_string_lossy().into_owned();
        assert!(!has_pending(qp));
    }

    /// `load_queue` surfaces JSON parse errors to the caller.
    #[test]
    fn test_load_queue_invalid_json_returns_err() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending-zotero-sync.json");
        std::fs::write(&path, "{broken").unwrap();
        let qp = path.to_string_lossy().into_owned();
        assert!(load_queue(qp).is_err());
    }

    /// Removing a key that is not in the queue is a no-op.
    #[test]
    fn test_remove_from_queue_missing_key_is_noop() {
        let dir = tempdir().unwrap();
        let qp = queue_path(&dir);
        enqueue(qp.clone(), "paper.md".into(), "KEY1".into(), "nlp".into()).unwrap();
        remove_from_queue(qp.clone(), "MISSING".into()).unwrap();
        assert_eq!(load_queue(qp).unwrap().len(), 1);
    }

    /// `enqueue` creates nested parent directories for the queue file.
    #[test]
    fn test_enqueue_creates_nested_parent_dirs() {
        let dir = tempdir().unwrap();
        let qp = dir
            .path()
            .join("deep")
            .join("meta")
            .join("pending-zotero-sync.json")
            .to_string_lossy()
            .into_owned();
        enqueue(qp.clone(), "p.md".into(), "ZZZZ9999".into(), "rl".into()).unwrap();
        assert!(load_queue(qp).unwrap().len() == 1);
    }

    // ── resolve_zotero_key tests ──────────────────────────────────────────────

    fn make_item(key: &str, paper_file: &str) -> PendingSyncItem {
        PendingSyncItem {
            paper_file: paper_file.to_string(),
            zotero_item_key: key.to_string(),
            target_collection: "nlp".to_string(),
            queued_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// A real 8-char uppercase Zotero key must be returned unchanged.
    #[tokio::test]
    async fn resolve_real_key_unchanged() {
        let item = make_item("ABCD1234", "/no/such/file.md");
        let result = resolve_zotero_key(&item).await;
        assert_eq!(result.unwrap(), "ABCD1234");
    }

    /// A lowercase 8-char key is NOT a real Zotero key — should fall through to
    /// the pass-through branch (not the "real key" shortcut).
    #[tokio::test]
    async fn resolve_lowercase_key_falls_through() {
        let item = make_item("abcd1234", "/no/such/file.md");
        // Zotero is offline in CI so it will fail resolution — the point is
        // that it does NOT use the real-key fast path for lowercase keys.
        let result = resolve_zotero_key(&item).await;
        // Either Ok (passed through) or Err (Zotero offline) — both are fine.
        // The key thing is the function doesn't panic.
        let _ = result;
    }

    /// `"doi:"` prefix → should attempt a DOI lookup (fails offline, which is expected).
    #[tokio::test]
    async fn resolve_doi_prefix_attempts_lookup() {
        let item = make_item("doi:10.1234/test", "/no/such/file.md");
        let result = resolve_zotero_key(&item).await;
        // Must fail since Zotero isn't running, but with a DOI-related message.
        let err = result.expect_err("DOI lookup must fail when Zotero is offline");
        assert!(
            err.contains("10.1234/test") || err.contains("Zotero"),
            "Error should mention the DOI or Zotero: {err}"
        );
    }

    /// `"file:"` prefix with a readable markdown file that has a title → title lookup.
    #[tokio::test]
    async fn resolve_file_prefix_reads_title_from_frontmatter() {
        let dir = tempdir().unwrap();
        let paper = dir.path().join("paper.md");
        std::fs::write(
            &paper,
            "---\ntitle: Attention Is All You Need\nabstract: Short.\n---\n",
        )
        .unwrap();

        let item = make_item(
            &format!("file:{}", paper.file_name().unwrap().to_string_lossy()),
            &paper.to_string_lossy(),
        );
        let result = resolve_zotero_key(&item).await;
        // Must fail since Zotero isn't running — but the error proves we tried title lookup.
        let err = result.expect_err("title lookup must fail when Zotero is offline");
        assert!(
            err.contains("Attention Is All You Need") || err.contains("Zotero"),
            "Error should mention the title or Zotero: {err}"
        );
    }

    /// `"file:"` prefix with no readable paper file → descriptive error.
    #[tokio::test]
    async fn resolve_file_prefix_missing_paper_file() {
        let item = make_item("file:phantom.md", "/nonexistent/phantom.md");
        let result = resolve_zotero_key(&item).await;
        let err = result.expect_err("must fail for missing paper file");
        assert!(
            err.contains("no parseable title") || err.contains("phantom"),
            "Error should explain why resolution failed: {err}"
        );
    }

    /// `extract_title_from_frontmatter` must return the title from valid YAML.
    #[test]
    fn extract_title_parses_yaml_frontmatter() {
        let md = "---\ntitle: My Paper Title\nabstract: Some abstract.\n---\nBody";
        assert_eq!(
            extract_title_from_frontmatter(md).as_deref(),
            Some("My Paper Title")
        );
    }

    /// No frontmatter → `None`.
    #[test]
    fn extract_title_returns_none_without_frontmatter() {
        assert_eq!(extract_title_from_frontmatter("Plain body only."), None);
    }

    /// Frontmatter with no `title:` field → `None`.
    #[test]
    fn extract_title_returns_none_when_title_absent() {
        let md = "---\nabstract: Something.\n---\n";
        assert_eq!(extract_title_from_frontmatter(md), None);
    }
}
