//! Gemini API client — model: gemini-2.5-pro.
//!
//! The API key is ALWAYS read from the OS Keychain via `crate::keychain`.
//! It is never accepted as a function argument or read from any file.
//!
//! # Tauri commands exposed
//! | Command            | Returns                 | Description                          |
//! |--------------------|-------------------------|--------------------------------------|
//! | `test_connection`  | `Result<bool, String>`  | Validate key with a minimal API call |
//! | `call_gemini`      | `()`                    | Stream chat reply via Tauri events   |
//! | `classify_paper`   | `Result<String, String>`| Kebab-case category from paper text  |
//!
//! ## Streaming events emitted by `call_gemini`
//! | Event                  | Payload         |
//! |------------------------|-----------------|
//! | `gemini-stream`        | `String` chunk  |
//! | `gemini-stream-done`   | `null`          |
//! | `gemini-stream-error`  | `String` reason |

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::Emitter;

const MODEL: &str = "gemini-2.5-pro";
const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
/// Hard timeout for a single HTTP request (streaming included).
const REQUEST_TIMEOUT_SECS: u64 = 180;
/// Max body excerpt length sent to Gemini for classification.
const BODY_EXCERPT_CHARS: usize = 2_000;

// ── Public input type ──────────────────────────────────────────────────────

/// A single conversation turn.  Sent from JS as `{ role, text }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// `"user"` or `"model"`
    pub role: String,
    pub text: String,
}

// ── Internal wire types (Gemini REST API) ─────────────────────────────────

#[derive(Debug, Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Debug, Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
}

// Non-streaming response
#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Vec<NsCandidate>,
}

#[derive(Debug, Deserialize)]
struct NsCandidate {
    content: NsContent,
}

#[derive(Debug, Deserialize)]
struct NsContent {
    parts: Vec<NsPart>,
}

#[derive(Debug, Deserialize)]
struct NsPart {
    text: String,
}

// Streaming SSE response (same shape, but all fields optional)
#[derive(Debug, Deserialize)]
struct SseChunk {
    candidates: Option<Vec<SseCandidate>>,
}

#[derive(Debug, Deserialize)]
struct SseCandidate {
    content: Option<NsContent>,
    // present on the final chunk; we don't act on it but capture it for completeness
    #[serde(rename = "finishReason")]
    _finish_reason: Option<String>,
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn build_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .expect("failed to build reqwest client")
}

fn to_gemini_contents(messages: Vec<Message>) -> Vec<GeminiContent> {
    messages
        .into_iter()
        .map(|m| GeminiContent {
            role: m.role,
            parts: vec![GeminiPart { text: m.text }],
        })
        .collect()
}

fn extract_ns_text(resp: &GeminiResponse) -> Option<String> {
    resp.candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .map(|p| p.text.clone())
}

/// Normalise Gemini's free-form category output → strict lower-case kebab-case.
///
/// Handles quoted strings, trailing punctuation, spaces, underscores, and
/// mixed case that the model might return despite the prompt instructions.
fn normalise_category(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '.')
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || c == '_' || c == '/' || c == '\\')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

// ── Tauri commands ─────────────────────────────────────────────────────────

/// Validate the stored Gemini API key by sending a minimal API request.
///
/// Returns `Ok(true)` if the key is accepted, or an error string describing
/// what went wrong (no key stored, network error, HTTP 4xx/5xx, etc.).
#[tauri::command]
pub async fn test_connection() -> Result<bool, String> {
    let api_key = crate::keychain::get_key_inner()
        .map_err(|e| format!("No API key in keychain: {e}"))?;

    let client = build_client();
    let url = format!("{API_BASE}/{MODEL}:generateContent?key={api_key}");

    let body = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart {
                // Shortest possible prompt that forces a text response
                text: "Reply with exactly one word: ok".into(),
            }],
        }],
    };

    client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Gemini API error: {e}"))?;

    Ok(true)
}

/// Stream a Gemini conversation to the frontend.
///
/// Reads the API key from the OS Keychain.  All status (including errors)
/// is communicated via Tauri events — this command always returns `()`.
///
/// Events emitted:
/// * `"gemini-stream"`       — one text chunk (String)
/// * `"gemini-stream-done"`  — stream finished normally (null payload)
/// * `"gemini-stream-error"` — error description (String)
#[tauri::command]
pub async fn call_gemini(messages: Vec<Message>, window: tauri::WebviewWindow) {
    // ── 1. Retrieve API key ───────────────────────────────────────────────
    let api_key = match crate::keychain::get_key_inner() {
        Ok(k) => k,
        Err(e) => {
            let _ = window.emit("gemini-stream-error", format!("Keychain error: {e}"));
            return;
        }
    };

    let client = build_client();
    let url = format!(
        "{API_BASE}/{MODEL}:streamGenerateContent?key={api_key}&alt=sse"
    );
    let body = GeminiRequest {
        contents: to_gemini_contents(messages),
    };

    // ── 2. Open streaming connection ──────────────────────────────────────
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            let _ = window.emit("gemini-stream-error", format!("Network error: {e}"));
            return;
        }
    };

    if let Err(e) = resp.error_for_status_ref() {
        let _ = window.emit("gemini-stream-error", format!("Gemini API error: {e}"));
        return;
    }

    // ── 3. Parse SSE stream line-by-line ──────────────────────────────────
    // The Gemini streaming endpoint sends Server-Sent Events:
    //   data: {json}\n
    //   \n
    // We accumulate bytes in `buf` and drain one line at a time so that
    // chunks that span multiple network packets are handled correctly.
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    loop {
        match stream.next().await {
            Some(Ok(bytes)) => {
                buf.push_str(&String::from_utf8_lossy(&bytes));

                // Drain every complete line from the front of the buffer.
                while let Some(nl) = buf.find('\n') {
                    let line = buf[..nl].to_string();
                    buf = buf[nl + 1..].to_string();
                    let line = line.trim();

                    // SSE data lines start with "data: "
                    let Some(json_str) = line.strip_prefix("data: ") else {
                        continue; // blank line, "event: ...", comments — skip
                    };

                    if json_str == "[DONE]" {
                        // Some SSE endpoints send an explicit [DONE] sentinel.
                        continue;
                    }

                    // Decode the JSON chunk and emit any text delta.
                    if let Ok(chunk) = serde_json::from_str::<SseChunk>(json_str) {
                        if let Some(text) = chunk
                            .candidates
                            .as_deref()
                            .and_then(|cs| cs.first())
                            .and_then(|c| c.content.as_ref())
                            .and_then(|c| c.parts.first())
                            .map(|p| p.text.clone())
                        {
                            let _ = window.emit("gemini-stream", text);
                        }
                    }
                }
            }
            Some(Err(e)) => {
                let _ = window.emit("gemini-stream-error", format!("Stream read error: {e}"));
                return;
            }
            None => break, // stream exhausted normally
        }
    }

    let _ = window.emit("gemini-stream-done", ());
}

/// Classify a research paper and return a lower-case kebab-case category string.
///
/// The category is the *only* output — no explanation, no punctuation.
/// Gemini is given title + abstract + a body excerpt for best accuracy.
///
/// # Errors
/// Returns an error string if:
/// * The API key is missing from the keychain
/// * The network or Gemini API returns an error
/// * Gemini returns an empty or unparseable category
#[tauri::command]
pub async fn classify_paper(
    title: String,
    // `abstract` is a Rust reserved word; `r#abstract` is the idiomatic
    // workaround.  The Tauri macro serialises this as "abstract" on the wire,
    // so the JS caller still writes: invoke('classify_paper', { title, abstract, body })
    r#abstract: String,
    body: String,
) -> Result<String, String> {
    let api_key = crate::keychain::get_key_inner()
        .map_err(|e| format!("No API key in keychain: {e}"))?;

    let client = build_client();

    // Truncate body to stay within token budget.
    let body_excerpt: String = body.chars().take(BODY_EXCERPT_CHARS).collect();

    let prompt = format!(
        "Classify the following research paper into a single lower-case kebab-case \
         category string (examples: \"large-language-models\", \
         \"reinforcement-learning\", \"computer-vision\", \"multimodal-learning\", \
         \"graph-neural-networks\"). \
         Rules:\n\
         - Return ONLY the category string.\n\
         - No spaces, no punctuation, no explanation.\n\
         - Use hyphens between words.\n\
         - All lower-case.\n\n\
         Title: {title}\n\n\
         Abstract: {abstract}\n\n\
         Body excerpt:\n{body_excerpt}"
    );

    let url = format!("{API_BASE}/{MODEL}:generateContent?key={api_key}");
    let request_body = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart { text: prompt }],
        }],
    };

    let resp: GeminiResponse = client
        .post(&url)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Gemini API error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    let raw = extract_ns_text(&resp).unwrap_or_default();
    let category = normalise_category(&raw);

    if category.is_empty() {
        return Err(format!(
            "Gemini returned an empty category (raw: {:?})",
            raw
        ));
    }

    Ok(category)
}
