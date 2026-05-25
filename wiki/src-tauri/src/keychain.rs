//! Keychain access — OS Keychain via the `keyring` crate.
//!
//! Service name : "llm-wiki"
//! Key name     : "llm-wiki-gemini-key"   (matches CLAUDE.md spec)
//!
//! Internal helpers (pub(crate)) are used by `gemini.rs` and other modules
//! that need the raw key without going through a Tauri command.

use keyring::Entry;

const SERVICE: &str = "llm-wiki";
const KEY_NAME: &str = "llm-wiki-gemini-key";

// ── Internal helpers ──────────────────────────────────────────────────────

/// Read the key from the OS Keychain.
/// Used by `crate::gemini` and the organiser — never exposed to JS directly.
pub(crate) fn get_key_inner() -> Result<String, keyring::Error> {
    Entry::new(SERVICE, KEY_NAME)?.get_password()
}

fn set_key_inner(key: &str) -> Result<(), keyring::Error> {
    Entry::new(SERVICE, KEY_NAME)?.set_password(key)
}

fn delete_key_inner() -> Result<(), keyring::Error> {
    Entry::new(SERVICE, KEY_NAME)?.delete_credential()
}

// ── Tauri commands ─────────────────────────────────────────────────────────

/// Store the Gemini API key in the OS Keychain.
///
/// Errors if `key` is blank or the OS keychain store fails.
/// The key is NEVER written to any file.
#[tauri::command]
pub fn save_api_key(key: String) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("API key must not be empty".into());
    }
    set_key_inner(key.trim()).map_err(|e| e.to_string())
}

/// Retrieve the stored Gemini API key from the OS Keychain.
#[tauri::command]
pub fn get_api_key() -> Result<String, String> {
    get_key_inner().map_err(|e| e.to_string())
}

/// Delete the Gemini API key from the OS Keychain.
#[tauri::command]
pub fn delete_api_key() -> Result<(), String> {
    delete_key_inner().map_err(|e| e.to_string())
}

/// Return `true` if a non-empty Gemini API key is present in the OS Keychain.
///
/// This command never returns an error — a missing or unreadable key → `false`.
/// Used by the onboarding flow to decide whether to show the key-input step.
#[tauri::command]
pub fn has_api_key() -> bool {
    match get_key_inner() {
        Ok(k) => !k.trim().is_empty(),
        Err(_) => false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{delete_key_inner, set_key_inner};
    use super::{delete_api_key, get_api_key, has_api_key, save_api_key};
    use std::sync::Mutex;

    /// All keychain tests touch the same OS credential slot — run them serially
    /// to prevent races between parallel test threads.
    static KC_LOCK: Mutex<()> = Mutex::new(());

    /// Returns `true` if the OS keychain is unreachable in this environment
    /// (e.g. WSL without a gnome-keyring / secret-service daemon).
    /// When `true`, the calling test should skip rather than fail.
    fn keychain_unavailable() -> bool {
        match set_key_inner("__probe__") {
            Ok(_) => {
                let _ = delete_key_inner();
                false
            }
            Err(_) => true,
        }
    }

    /// Save a known value, retrieve it, assert they match.
    #[test]
    fn test_save_and_retrieve_key() {
        let _g = KC_LOCK.lock().unwrap();
        if keychain_unavailable() {
            eprintln!("SKIP test_save_and_retrieve_key: OS keychain not available");
            return;
        }
        save_api_key("test-key-abc123".to_string()).unwrap();
        let got = get_api_key().unwrap();
        assert_eq!(got, "test-key-abc123");
        // Clean up so other tests start fresh.
        let _ = delete_api_key();
    }

    /// Save then delete; `has_api_key` must return `false` afterwards.
    #[test]
    fn test_delete_key() {
        let _g = KC_LOCK.lock().unwrap();
        if keychain_unavailable() {
            eprintln!("SKIP test_delete_key: OS keychain not available");
            return;
        }
        save_api_key("to-be-deleted".to_string()).unwrap();
        delete_api_key().unwrap();
        assert!(!has_api_key(), "has_api_key() should be false after deletion");
    }

    /// Blank keys must be rejected before touching the OS keychain.
    #[test]
    fn test_save_api_key_rejects_blank() {
        let result = save_api_key("   ".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    /// `has_api_key` returns true after a successful save.
    #[test]
    fn test_has_api_key_after_save() {
        let _g = KC_LOCK.lock().unwrap();
        if keychain_unavailable() {
            eprintln!("SKIP test_has_api_key_after_save: OS keychain not available");
            return;
        }
        save_api_key("presence-check-key".to_string()).unwrap();
        assert!(has_api_key());
        let _ = delete_api_key();
    }

    /// `get_api_key` must return `Err` when no key has been stored.
    #[test]
    fn test_get_nonexistent_key() {
        let _g = KC_LOCK.lock().unwrap();
        if keychain_unavailable() {
            eprintln!("SKIP test_get_nonexistent_key: OS keychain not available");
            return;
        }
        // Ensure the slot is empty before we probe.
        let _ = delete_api_key();
        let result = get_api_key();
        assert!(
            result.is_err(),
            "Expected Err for missing key, got Ok({:?})",
            result.ok()
        );
    }
}
