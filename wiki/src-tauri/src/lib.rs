pub mod content;
pub mod gemini;
pub mod keychain;
pub mod organizer;
pub mod pending_sync;
pub mod transaction;
pub mod zotero;

use tauri::{Emitter, Manager};

// ── Shared app state ──────────────────────────────────────────────────────────

/// Mutable state shared between Tauri commands and the background watcher.
///
/// `content_root` is set once by the frontend after onboarding completes
/// (`set_content_root` command).  The Zotero watcher reads it to derive the
/// pending-sync queue path when it auto-triggers `sync_all` on reconnect.
#[derive(Default)]
pub struct AppState {
    pub content_root: std::sync::Mutex<Option<String>>,
}

/// Store the content-root path in app state.
///
/// Called by the frontend after the user selects their content folder.
/// The path is used by the Zotero watcher to locate the pending-sync queue.
#[tauri::command]
fn set_content_root(
    state: tauri::State<AppState>,
    path: String,
) -> Result<(), String> {
    *state
        .content_root
        .lock()
        .map_err(|e| format!("State lock poisoned: {e}"))? = Some(path);
    Ok(())
}

/// Return the currently stored content-root path, or `null` if not set.
#[tauri::command]
fn get_content_root(state: tauri::State<AppState>) -> Option<String> {
    state
        .content_root
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

// ── Zotero background watcher ─────────────────────────────────────────────────

/// Poll Zotero every [`zotero::POLL_INTERVAL_SECS`] seconds.
///
/// On every tick:
/// * emits `"zotero-status"` to all webview windows
///
/// On transition Disconnected → Connected:
/// * checks if the pending-sync queue is non-empty
/// * if so, spawns [`pending_sync::sync_all`] on the "main" window
///
/// The watcher runs for the lifetime of the process and never returns.
async fn zotero_watcher(app: tauri::AppHandle) {
    use tokio::time::{interval, Duration, MissedTickBehavior};

    let mut ticker = interval(Duration::from_secs(zotero::POLL_INTERVAL_SECS));
    // Don't pile up missed ticks if a sync_all run takes longer than 30 s.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("zotero_watcher: failed to build HTTP client");

    let mut was_connected = false;

    loop {
        ticker.tick().await;

        let is_connected = zotero::ping(&http).await;

        // Emit connectivity status to all open windows.
        let status = if is_connected {
            zotero::ZoteroStatus::Connected
        } else {
            zotero::ZoteroStatus::Disconnected
        };
        let _ = app.emit("zotero-status", &status);

        // Auto-trigger pending-sync on every Disconnected → Connected transition.
        if is_connected && !was_connected {
            let content_root = app
                .state::<AppState>()
                .content_root
                .lock()
                .ok()
                .and_then(|g| g.clone());

            if let Some(root) = content_root {
                let queue_path =
                    format!("{root}/meta/pending-zotero-sync.json");

                if pending_sync::has_pending(queue_path.clone()) {
                    if let Some(window) = app.get_webview_window("main") {
                        // Spawn so the watcher tick is not blocked by sync_all.
                        let qp = queue_path.clone();
                        tokio::spawn(async move {
                            let _ = pending_sync::sync_all(qp, window).await;
                        });
                    }
                }
            }
        }

        was_connected = is_connected;
    }
}

// ── App entry point ───────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Spawn the Zotero connectivity watcher as a long-lived background task.
            let app_handle = app.handle().clone();
            tokio::spawn(async move {
                zotero_watcher(app_handle).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // ── App state ─────────────────────────────────────────────────
            set_content_root,
            get_content_root,
            // ── Keychain ──────────────────────────────────────────────────
            keychain::save_api_key,
            keychain::get_api_key,
            keychain::delete_api_key,
            keychain::has_api_key,
            // ── Gemini ────────────────────────────────────────────────────
            gemini::test_connection,
            gemini::call_gemini,
            gemini::classify_paper,
            // ── Zotero ────────────────────────────────────────────────────
            zotero::check_status,
            zotero::get_item_by_doi,
            zotero::get_current_collection,
            zotero::update_collection,
            zotero::wait_for_zotmoov,
            // ── Pending sync ──────────────────────────────────────────────
            pending_sync::enqueue,
            pending_sync::load_queue,
            pending_sync::remove_from_queue,
            pending_sync::has_pending,
            pending_sync::sync_all,
            // ── Organiser ─────────────────────────────────────────────────
            organizer::process_paper,
            // ── Content reader ────────────────────────────────────────────
            content::list_categories,
            content::list_papers_in_category,
            content::list_recent_papers,
            content::read_paper_file,
            content::find_paper_category,
            content::list_unclassified,
            content::read_backlinks,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod state_tests {
    use super::AppState;

    #[test]
    fn app_state_stores_content_root() {
        let state = AppState::default();
        *state.content_root.lock().unwrap() = Some("/data/content".into());
        let got = state.content_root.lock().unwrap().clone();
        assert_eq!(got.as_deref(), Some("/data/content"));
    }
}
