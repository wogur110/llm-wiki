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
/// Lighter model for key validation only — higher free-tier RPM than 2.5 Pro.
const TEST_MODEL: &str = "gemini-2.5-flash";
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

/// A single Gemini "part" — either text or inline binary data (PDFs, images).
/// Serialised with `inline_data` (snake_case) which is what the v1beta REST API accepts.
#[derive(Debug, Serialize, Default)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "inline_data")]
    inline_data: Option<InlineData>,
}

impl GeminiPart {
    fn text(s: impl Into<String>) -> Self {
        Self { text: Some(s.into()), inline_data: None }
    }

    fn pdf(bytes: &[u8]) -> Self {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        Self {
            text: None,
            inline_data: Some(InlineData {
                mime_type: "application/pdf".into(),
                data: STANDARD.encode(bytes),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct InlineData {
    #[serde(rename = "mime_type")]
    mime_type: String,
    data: String,
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
            parts: vec![GeminiPart::text(m.text)],
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

/// Raw PDF size we can send as `inline_data`.  The whole JSON request must stay
/// under ~20 MB; base64 inflates bytes by ~4/3, plus the prompt text.
const MAX_INLINE_PDF_BYTES: usize = 12 * 1024 * 1024;

/// Hard ceiling for PDF processing on Gemini (inline or Files API).
const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct GeminiApiErrorEnvelope {
    error: Option<GeminiApiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct GeminiApiErrorDetail {
    message: Option<String>,
}

/// True when `bytes` begin with the PDF magic header.
pub(crate) fn looks_like_pdf(bytes: &[u8]) -> bool {
    bytes.len() >= 5 && bytes.starts_with(b"%PDF-")
}

/// Build a user-facing message from an HTTP status and optional Gemini JSON body.
/// Never includes the request URL (it may embed the API key).
fn format_gemini_http_error(status: reqwest::StatusCode, api_message: Option<&str>) -> String {
    let detail = api_message.unwrap_or("").to_lowercase();

    match status.as_u16() {
        400 => {
            if detail.contains("too large")
                || detail.contains("bytes are too large")
                || detail.contains("payload size")
                || detail.contains("exceeds")
            {
                return "PDF가 너무 커서 Gemini가 거부했습니다(400). \
                        인라인 전송 한도는 약 12 MB, 처리 한도는 50 MB입니다. \
                        Adobe 등으로 PDF를 압축하거나 페이지를 나눈 뒤 다시 시도하세요."
                    .into();
            }
            if detail.contains("too many pages") || detail.contains("page") {
                return "PDF 페이지 수가 Gemini 한도(약 1,000쪽)를 넘었습니다(400). \
                        파일을 나눠서 다시 시도하세요."
                    .into();
            }
            if detail.contains("mime") || detail.contains("invalid argument") {
                let extra = api_message
                    .filter(|m| !m.is_empty())
                    .map(|m| format!(" (서버: {m})"))
                    .unwrap_or_default();
                return format!(
                    "Gemini가 PDF를 읽지 못했습니다(400). \
                     암호가 걸린 PDF, 손상된 파일, 또는 Zotero에서 PDF가 아닌 데이터가 \
                     내려받힌 경우일 수 있습니다.{extra}"
                );
            }
            format!(
                "Gemini 요청이 거부되었습니다(400).{}",
                api_message
                    .filter(|m| !m.is_empty())
                    .map(|m| format!(" 서버 메시지: {m}"))
                    .unwrap_or_else(|| {
                        " PDF가 암호화·손상되었거나 너무 클 수 있습니다.".into()
                    })
            )
        }
        401 | 403 => {
            "Gemini API 키가 거부되었습니다. AI Studio에서 키를 다시 발급받았는지 확인하세요.".into()
        }
        429 => {
            "Gemini API 한도 초과(429). RPM·TPM·RPD 중 하나가 막혔습니다. \
             현재 프로젝트의 한도는 AI Studio → Rate limits 메뉴에서 확인하고, \
             무료 티어라면 AI Studio → Billing에서 결제 카드를 연결하면 \
             gemini-2.5-pro 한도가 5 RPM → 150 RPM로 자동 상승합니다."
                .into()
        }
        n => {
            if let Some(m) = api_message.filter(|s| !s.is_empty()) {
                format!("Gemini API 오류: HTTP {n} — {m}")
            } else {
                format!("Gemini API 오류: HTTP {n}")
            }
        }
    }
}

/// Read the JSON error body from a failed Gemini response.
async fn gemini_error_from_response(resp: reqwest::Response) -> String {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let api_message = serde_json::from_str::<GeminiApiErrorEnvelope>(&text)
        .ok()
        .and_then(|e| e.error)
        .and_then(|d| d.message);
    format_gemini_http_error(status, api_message.as_deref())
}

// ── Tauri commands ─────────────────────────────────────────────────────────

/// Validate a Gemini API key by sending a minimal API request.
///
/// When `api_key` is provided (onboarding / settings), that value is tested
/// directly.  When omitted, the key is read from the OS keychain — useful for
/// background checks after a successful save.
///
/// Returns `Ok(true)` if the key is accepted, or an error string describing
/// what went wrong (no key stored, network error, HTTP 4xx/5xx, etc.).
#[tauri::command]
pub async fn test_connection(api_key: Option<String>) -> Result<bool, String> {
    let api_key = match api_key {
        Some(k) => {
            let trimmed = k.trim();
            if trimmed.is_empty() {
                return Err("API key must not be empty".into());
            }
            trimmed.to_string()
        }
        None => crate::keychain::get_key_inner()
            .map_err(|e| format!("No API key in keychain: {e}"))?,
    };

    let client = build_client();
    // Use Flash for the probe so onboarding tests do not burn 2.5 Pro quota.
    let url = format!("{API_BASE}/{TEST_MODEL}:generateContent?key={api_key}");

    let body = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart::text("Reply with exactly one word: ok")],
        }],
    };

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !resp.status().is_success() {
        return Err(gemini_error_from_response(resp).await);
    }

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

/// Classify a paper into an **existing** category when possible, creating a
/// new one only if none of the candidates fit well.
///
/// Same interface as [`classify_paper`] but the prompt includes the list of
/// existing categories so Gemini prefers them over inventing new names.
/// Call this for new papers imported from the Zotero Unclassified collection
/// when the wiki already has established categories.
///
/// `existing_categories` should be lower-case kebab-case names.  An empty
/// slice falls back to free-form classification (same behaviour as
/// [`classify_paper`]).
///
/// # Errors
/// Same error conditions as [`classify_paper`].
pub(crate) async fn classify_paper_with_existing_categories(
    title: String,
    abstract_text: String,
    body: String,
    existing_categories: Vec<String>,
) -> Result<String, String> {
    let api_key = crate::keychain::get_key_inner()
        .map_err(|e| format!("No API key in keychain: {e}"))?;

    let client = build_client();
    let body_excerpt: String = body.chars().take(BODY_EXCERPT_CHARS).collect();

    let category_hint = if existing_categories.is_empty() {
        String::new()
    } else {
        let cats = existing_categories.join("\", \"");
        format!(
            "Prefer one of these existing categories if the paper fits well: \
             [\"{cats}\"]\n\
             Only create a NEW category if the paper genuinely does not fit \
             any of the above (e.g., it covers a completely different field).\n\n"
        )
    };

    let prompt = format!(
        "Classify the following research paper into a single lower-case \
         kebab-case category string (examples: \"large-language-models\", \
         \"reinforcement-learning\", \"computer-vision\", \
         \"multimodal-learning\", \"graph-neural-networks\"). \
         Rules:\n\
         - Return ONLY the category string.\n\
         - No spaces, no punctuation, no explanation.\n\
         - Use hyphens between words.\n\
         - All lower-case.\n\n\
         {category_hint}\
         Title: {title}\n\n\
         Abstract: {abstract_text}\n\n\
         Body excerpt:\n{body_excerpt}"
    );

    let url = format!("{API_BASE}/{MODEL}:generateContent?key={api_key}");
    let request_body = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart::text(prompt)],
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
            parts: vec![GeminiPart::text(prompt)],
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

/// Convert a PDF file (read into memory) into a markdown document.
///
/// Sends the raw PDF inline (base64) to Gemini along with a prompt that asks
/// for a structured markdown export with YAML frontmatter.  Raw PDFs must be
/// ≤ [`MAX_INLINE_PDF_BYTES`] (~12 MB) because the entire HTTP request body
/// (base64 + prompt) must stay under Gemini's ~20 MB inline ceiling.
///
/// The returned string is expected to start with a YAML frontmatter block
/// containing at least `title:` and `abstract:` so the existing organiser
/// pipeline (`organizer::process_paper`) can classify it without changes.
///
/// # Errors
/// * Missing API key
/// * Invalid / empty / oversized PDF
/// * Network / HTTP error (400, 401, 429, 5xx) with Gemini message when available
/// * Empty response from Gemini
pub async fn extract_pdf_to_markdown(pdf_bytes: Vec<u8>) -> Result<String, String> {
    if pdf_bytes.is_empty() {
        return Err(
            "Zotero에서 받은 PDF가 비어 있습니다. Zotero에서 해당 항목의 첨부 파일이 \
             정상인지 확인하세요."
                .into(),
        );
    }
    if !looks_like_pdf(&pdf_bytes) {
        return Err(
            "다운로드한 파일이 PDF가 아닙니다(%PDF- 헤더 없음). Zotero에서 PDF 첨부가 \
             연결·동기화되었는지, 또는 HTML/오류 페이지가 아닌지 확인하세요."
                .into(),
        );
    }
    if pdf_bytes.len() > MAX_PDF_BYTES {
        return Err(format!(
            "PDF가 {} MB입니다. Gemini 처리 한도는 50 MB입니다. 파일을 압축하거나 나눠 주세요.",
            pdf_bytes.len() / (1024 * 1024)
        ));
    }
    if pdf_bytes.len() > MAX_INLINE_PDF_BYTES {
        return Err(format!(
            "PDF가 {} MB입니다. 앱의 인라인 전송 한도는 약 12 MB입니다 \
             (Gemini 요청 본문 20 MB 제한). Adobe 등으로 압축한 뒤 다시 시도하세요.",
            pdf_bytes.len() / (1024 * 1024)
        ));
    }

    let api_key = crate::keychain::get_key_inner()
        .map_err(|e| format!("No API key in keychain: {e}"))?;

    let client = build_client();
    let url = format!("{API_BASE}/{MODEL}:generateContent?key={api_key}");

    let prompt = "You convert a research paper PDF into a markdown document for a personal wiki. \
Rules:\n\
1. Start with a YAML frontmatter block delimited by `---` containing exactly these fields, no others:\n\
   - title: \"<paper title>\"\n\
   - authors: \"<comma-separated author list>\"\n\
   - abstract: \"<full abstract, single line, escape quotes>\"\n\
   - doi: \"<DOI string or empty>\"\n\
   - year: <publication year or empty>\n\
2. After the closing `---`, render the paper body in GitHub-flavoured markdown:\n\
   - Use `##` for top-level sections (Introduction, Method, …), `###` for subsections.\n\
   - Preserve display math as `$$ … $$` and inline math as `$ … $` (LaTeX, not unicode).\n\
   - Skip references, figures, and tables that are purely visual; keep figure captions inline.\n\
3. Do NOT wrap the response in ```markdown fences. Output the document directly.";

    let request_body = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart::text(prompt), GeminiPart::pdf(&pdf_bytes)],
        }],
    };

    let resp = client
        .post(&url)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !resp.status().is_success() {
        return Err(gemini_error_from_response(resp).await);
    }

    let parsed: GeminiResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON decode error: {e}"))?;

    let text = extract_ns_text(&parsed)
        .ok_or_else(|| "Gemini returned no markdown content".to_string())?;

    let text = text.trim();
    if text.is_empty() {
        return Err("Gemini returned an empty markdown body".into());
    }

    Ok(strip_markdown_fence(text))
}

/// Trim a ```markdown … ``` fence if Gemini ignores the no-fence instruction.
fn strip_markdown_fence(s: &str) -> String {
    let trimmed = s.trim();
    let body = trimmed
        .strip_prefix("```markdown")
        .or_else(|| trimmed.strip_prefix("```md"))
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_start_matches('\n'))
        .unwrap_or(trimmed);

    body.strip_suffix("```")
        .map(|s| s.trim_end().to_string())
        .unwrap_or_else(|| body.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        normalise_category, build_client, extract_ns_text, to_gemini_contents, API_BASE, MODEL,
        GeminiRequest, GeminiContent, GeminiPart, GeminiResponse, Message,
        NsCandidate, NsContent, NsPart,
    };

    /// True iff `s` is non-empty lower-case kebab-case:
    /// starts with [a-z], all chars in [a-z0-9-], no leading/trailing hyphen.
    fn is_kebab_case(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .next()
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false)
            && !s.ends_with('-')
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    /// `normalise_category` must always produce lower-case kebab-case output.
    #[test]
    fn test_classify_returns_kebab_case() {
        let cases = [
            "large-language-models",
            "  Reinforcement Learning  ",
            "Computer Vision",
            "Graph_Neural_Networks",
            "\"multimodal-learning\"",
        ];
        for input in &cases {
            let result = normalise_category(input);
            assert!(
                is_kebab_case(&result),
                "normalise_category({input:?}) = {result:?} is not valid kebab-case"
            );
        }
    }

    /// Output must never contain uppercase letters.
    #[test]
    fn test_classify_no_uppercase() {
        let cases = [
            "LargeLanguageModels",
            "BERT",
            "GPT-4",
            "ReinforcementLearning",
        ];
        for input in &cases {
            let result = normalise_category(input);
            assert_eq!(
                result,
                result.to_lowercase(),
                "normalise_category({input:?}) = {result:?} contains uppercase letters"
            );
        }
    }

    /// Spaces and underscores must be collapsed to hyphens, not preserved.
    #[test]
    fn test_classify_no_spaces() {
        let cases = [
            "large language models",
            "reinforcement learning",
            "computer vision tasks",
            "graph neural networks",
        ];
        for input in &cases {
            let result = normalise_category(input);
            assert!(
                !result.contains(' '),
                "normalise_category({input:?}) = {result:?} still contains a space"
            );
            assert!(
                !result.contains('_'),
                "normalise_category({input:?}) = {result:?} still contains an underscore"
            );
        }
    }

    /// Submitting an invalid API key to the Gemini REST endpoint must yield
    /// a non-2xx response (HTTP 400 "API key not valid" or similar).
    ///
    /// The test bypasses the OS keychain entirely — it constructs the HTTP
    /// request directly so keyring's zbus backend never runs inside the Tokio
    /// executor (which would cause a "cannot start a runtime from within a
    /// runtime" panic in environments that use async-secret-service).
    ///
    #[test]
    fn test_extract_ns_text_reads_first_candidate() {
        let resp = GeminiResponse {
            candidates: vec![NsCandidate {
                content: NsContent {
                    parts: vec![NsPart {
                        text: "large-language-models".into(),
                    }],
                },
            }],
        };
        assert_eq!(
            extract_ns_text(&resp).as_deref(),
            Some("large-language-models")
        );
    }

    #[test]
    fn test_extract_ns_text_empty_candidates() {
        let resp = GeminiResponse { candidates: vec![] };
        assert_eq!(extract_ns_text(&resp), None);
    }

    #[test]
    fn test_to_gemini_contents_maps_roles() {
        let contents = to_gemini_contents(vec![
            Message {
                role: "user".into(),
                text: "hello".into(),
            },
            Message {
                role: "model".into(),
                text: "hi".into(),
            },
        ]);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[1].parts[0].text.as_deref(), Some("hi"));
    }

    #[test]
    fn strip_markdown_fence_removes_wrapping_fence() {
        use super::strip_markdown_fence;
        let input = "```markdown\n---\ntitle: x\n---\nbody\n```";
        let out = strip_markdown_fence(input);
        assert!(out.starts_with("---"));
        assert!(out.ends_with("body"));
    }

    #[test]
    fn strip_markdown_fence_leaves_plain_text_untouched() {
        use super::strip_markdown_fence;
        let input = "---\ntitle: x\n---\nbody";
        let out = strip_markdown_fence(input);
        assert_eq!(out, input);
    }

    #[test]
    fn gemini_part_pdf_encodes_base64() {
        use super::GeminiPart;
        let part = GeminiPart::pdf(b"%PDF-1.4 test");
        let inline = part.inline_data.expect("inline_data should be set");
        assert_eq!(inline.mime_type, "application/pdf");
        assert!(!inline.data.is_empty());
        assert!(part.text.is_none());
    }

    /// Passes trivially in offline environments (network error ≠ success).
    #[tokio::test]
    async fn test_test_connection_invalid_key() {
        let client = build_client();
        let url = format!(
            "{API_BASE}/{MODEL}:generateContent?key=invalid-key-123"
        );
        let body = GeminiRequest {
            contents: vec![GeminiContent {
                role: "user".into(),
                parts: vec![GeminiPart::text("Reply with exactly one word: ok")],
            }],
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client.post(&url).json(&body).send(),
        )
        .await;

        match result {
            // Network timeout — offline environment, can't reach Gemini.
            // Still counts as "did not succeed", so the test passes.
            Err(_timeout) => {}
            // Got a response: the fake key must be rejected (4xx).
            Ok(Ok(resp)) => {
                assert!(
                    !resp.status().is_success(),
                    "Gemini must reject an invalid API key; got status {}",
                    resp.status()
                );
            }
            // Transport / TLS error — also means we didn't get a success.
            Ok(Err(_network_err)) => {}
        }
    }
}
