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

        // Delegate to the zotero module — find-or-create collection then PATCH.
        match crate::zotero::update_collection(
            item.zotero_item_key.clone(),
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
}
