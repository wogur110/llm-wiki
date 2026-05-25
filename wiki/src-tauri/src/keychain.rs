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
