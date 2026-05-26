//! On-demand AI summary generation for a single paper.
//!
//! Imports leave the `summary:` frontmatter field empty when the importer
//! has no AI-generated text to write.  This module fills that gap on user
//! request: it reads the paper's existing markdown, asks Gemini for a
//! short English summary based on whatever metadata is available
//! (title + abstract + body excerpt), and writes the result back into the
//! YAML frontmatter so subsequent loads pick it up without another API call.
//!
//! # Tauri command exposed
//! | Command            | Returns                  | Description                          |
//! |--------------------|--------------------------|--------------------------------------|
//! | `summarize_paper`  | `Result<String, String>` | Generate + persist AI summary        |
//!
//! The command is intentionally synchronous from the frontend's perspective
//! (no streaming).  Summaries are short (~5–10 sentences) so the latency is
//! dominated by Gemini round-trip time, not text-rendering cost.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum body excerpt length sent to Gemini.  Generous because we want the
/// summary to reflect the paper, not just the abstract.
const SUMMARY_BODY_CHARS: usize = 6_000;

/// Minimal Gemini wire types — duplicated here (rather than reused from
/// [`crate::gemini`]) because the existing structs in that module are
/// private.  Keeping a parallel small copy is cheaper than widening the API
/// surface of `gemini.rs` just for this one caller.
#[derive(Serialize)]
struct GContent<'a> {
    role: &'a str,
    parts: Vec<GPart<'a>>,
}

#[derive(Serialize)]
struct GPart<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct GRequest<'a> {
    contents: Vec<GContent<'a>>,
}

#[derive(Deserialize)]
struct GResponse {
    candidates: Vec<GCandidate>,
}

#[derive(Deserialize)]
struct GCandidate {
    content: GRespContent,
}

#[derive(Deserialize)]
struct GRespContent {
    parts: Vec<GRespPart>,
}

#[derive(Deserialize)]
struct GRespPart {
    text: String,
}

// ── File helpers ──────────────────────────────────────────────────────────

/// Split a markdown document into `(frontmatter_yaml, body)`.
///
/// Returns `(None, body)` if no leading `---` block is present.
pub(crate) fn split_frontmatter(markdown: &str) -> (Option<&str>, &str) {
    let trimmed = markdown.trim_start();
    let Some(after_first) = trimmed.strip_prefix("---") else {
        return (None, markdown);
    };
    let after_first = after_first.strip_prefix('\n').unwrap_or(after_first);
    let Some(end) = after_first.find("\n---") else {
        return (None, markdown);
    };
    let yaml = &after_first[..end];
    let rest = &after_first[end + 4..]; // skip "\n---"
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    (Some(yaml), rest)
}

/// Extract a YAML scalar value for `key:` from the frontmatter block.
///
/// Only handles plain and double-quoted scalars on a single line — which is
/// exactly the shape `pdf_import::build_metadata_markdown` produces.
fn yaml_scalar<'a>(yaml: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}:");
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(&needle) {
            let v = rest.trim();
            let unquoted = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(v);
            return Some(unquoted);
        }
    }
    None
}

/// Escape a free-form Gemini response for embedding in a YAML double-quoted
/// single-line scalar.  Matches the same rules as `pdf_import::escape_yaml_plain`.
pub(crate) fn escape_for_yaml(s: &str) -> String {
    let collapsed = s.replace('\r', " ").replace('\n', " ");
    let trimmed: String = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");
    trimmed.replace('"', "\u{201D}")
}

/// Insert or update the `summary:` line in a YAML frontmatter block.
///
/// * If the block already contains `summary:`, that line is replaced.
/// * Otherwise, a new line is appended immediately before the closing `---`.
pub(crate) fn upsert_summary(markdown: &str, summary: &str) -> String {
    let escaped = escape_for_yaml(summary);
    let new_line = format!("summary: \"{escaped}\"");

    let (Some(yaml), body) = split_frontmatter(markdown) else {
        // No frontmatter at all — synthesize one.
        return format!("---\n{new_line}\n---\n\n{markdown}", markdown = markdown.trim_start());
    };

    let mut found = false;
    let mut rebuilt_yaml = String::with_capacity(yaml.len() + new_line.len() + 8);
    for line in yaml.lines() {
        if line.trim_start().starts_with("summary:") {
            rebuilt_yaml.push_str(&new_line);
            rebuilt_yaml.push('\n');
            found = true;
        } else {
            rebuilt_yaml.push_str(line);
            rebuilt_yaml.push('\n');
        }
    }
    if !found {
        rebuilt_yaml.push_str(&new_line);
        rebuilt_yaml.push('\n');
    }

    format!("---\n{rebuilt_yaml}---\n{body}", body = if body.starts_with('\n') {
        body.to_string()
    } else {
        format!("\n{body}")
    })
}

// ── Gemini call ───────────────────────────────────────────────────────────

const MODEL: &str = "gemini-2.5-flash";
const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

async fn ask_gemini_for_summary(
    title: &str,
    abstract_text: &str,
    body_excerpt: &str,
) -> Result<String, String> {
    let api_key = crate::keychain::get_key_inner()
        .map_err(|e| format!("No Gemini API key in keychain: {e}"))?;

    let prompt = format!(
        "You are a research-note assistant.  Summarise the following paper in \
         5–8 sentences of fluent English.  Rules:\n\
         - Write in English only; use plain prose (no bullets, no numbering).\n\
         - Cover: the core problem, the proposed method, the main contributions, \
           and the significance of the results — in that order.\n\
         - If the body excerpt is short, infer from the abstract alone.\n\
         - Output plain text only — no markdown, no headings, no preamble.\n\n\
         Title: {title}\n\n\
         Abstract: {abstract_text}\n\n\
         Body excerpt:\n{body_excerpt}"
    );

    let body = GRequest {
        contents: vec![GContent {
            role: "user",
            parts: vec![GPart { text: &prompt }],
        }],
    };

    let url = format!("{API_BASE}/{MODEL}:generateContent?key={api_key}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Gemini API error (HTTP {status}): {text}"));
    }

    let parsed: GResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to decode response: {e}"))?;

    let text = parsed
        .candidates
        .into_iter()
        .next()
        .and_then(|c| c.content.parts.into_iter().next())
        .map(|p| p.text)
        .unwrap_or_default();

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Gemini returned an empty response.".into());
    }
    Ok(trimmed.to_string())
}

// ── Tauri command ─────────────────────────────────────────────────────────

/// Result returned by [`summarize_paper`] so the UI can refresh the drawer
/// without re-reading the file.
#[derive(Debug, Serialize)]
pub struct SummaryResult {
    /// The new summary text written to the file's frontmatter.
    pub summary: String,
}

/// Generate an AI summary for a paper and write it back to the markdown file.
///
/// `slug` is the canonical paper slug (filename minus `.md`); the category
/// folder is resolved via [`crate::content::find_paper_category`] so callers
/// only need to pass the slug.
///
/// The command always re-runs Gemini, even if a summary already exists —
/// callers (the drawer button) decide whether to expose the action.  After
/// success, the file on disk has its `summary:` frontmatter line updated.
#[tauri::command]
pub async fn summarize_paper(
    content_root: String,
    slug: String,
) -> Result<SummaryResult, String> {
    // ── 1. Locate the paper file ──────────────────────────────────────────
    let category = crate::content::find_paper_category(content_root.clone(), slug.clone())?;
    let path = PathBuf::from(&content_root)
        .join("papers")
        .join(&category)
        .join(format!("{slug}.md"));

    let original = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Cannot read paper file ({}): {e}", path.display()))?;

    // ── 2. Extract title + abstract + body excerpt ────────────────────────
    let (yaml_opt, body) = split_frontmatter(&original);
    let yaml = yaml_opt.unwrap_or("");
    let title = yaml_scalar(yaml, "title").unwrap_or("").trim().to_string();
    let abstract_text = yaml_scalar(yaml, "abstract")
        .unwrap_or("")
        .trim()
        .to_string();
    let body_excerpt: String = body.chars().take(SUMMARY_BODY_CHARS).collect();

    if title.is_empty() && abstract_text.is_empty() && body_excerpt.trim().is_empty() {
        return Err(
            "Nothing to summarise — title, abstract, and body are all empty. \
             Please fill in the abstract in Zotero first."
                .into(),
        );
    }

    // ── 3. Call Gemini ────────────────────────────────────────────────────
    let summary = ask_gemini_for_summary(&title, &abstract_text, &body_excerpt).await?;

    // ── 4. Persist back to disk ───────────────────────────────────────────
    let updated = upsert_summary(&original, &summary);
    tokio::fs::write(&path, updated)
        .await
        .map_err(|e| format!("Failed to write summary to file: {e}"))?;

    Ok(SummaryResult { summary })
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_frontmatter_extracts_yaml_block() {
        let md = "---\ntitle: A\nyear: 2020\n---\n\nBody here.\n";
        let (yaml, body) = split_frontmatter(md);
        assert_eq!(yaml, Some("title: A\nyear: 2020"));
        assert_eq!(body.trim_start(), "Body here.\n");
    }

    #[test]
    fn split_frontmatter_returns_none_when_missing() {
        let md = "No frontmatter here.";
        let (yaml, body) = split_frontmatter(md);
        assert_eq!(yaml, None);
        assert_eq!(body, md);
    }

    #[test]
    fn yaml_scalar_reads_quoted_and_plain_values() {
        let y = "title: \"Hello\"\nyear: 2020\n";
        assert_eq!(yaml_scalar(y, "title"), Some("Hello"));
        assert_eq!(yaml_scalar(y, "year"), Some("2020"));
        assert_eq!(yaml_scalar(y, "missing"), None);
    }

    #[test]
    fn upsert_summary_inserts_new_line_before_closing_delimiter() {
        let md = "---\ntitle: \"A\"\nyear: 2020\n---\n\nBody\n";
        let out = upsert_summary(md, "New summary.");
        assert!(out.contains("summary: \"New summary.\""));
        assert!(out.contains("title: \"A\""));
        assert!(out.contains("Body"));
        let summary_idx = out.find("summary:").unwrap();
        let closing_idx = out.rfind("---").unwrap();
        assert!(summary_idx < closing_idx);
    }

    #[test]
    fn upsert_summary_replaces_existing_line() {
        let md = "---\ntitle: \"A\"\nsummary: \"Old\"\n---\nBody";
        let out = upsert_summary(md, "Brand new summary.");
        assert!(out.contains("summary: \"Brand new summary.\""));
        assert!(!out.contains("Old"));
        // No duplicate summary lines.
        assert_eq!(out.matches("summary:").count(), 1);
    }

    #[test]
    fn upsert_summary_synthesises_frontmatter_when_missing() {
        let md = "Body only, no fm.";
        let out = upsert_summary(md, "added");
        assert!(out.starts_with("---\nsummary: \"added\"\n---\n"));
        assert!(out.contains("Body only, no fm."));
    }

    #[test]
    fn escape_for_yaml_collapses_whitespace_and_quotes() {
        let s = "Line 1\nLine 2\r\nwith \"quotes\"";
        let out = escape_for_yaml(s);
        assert!(!out.contains('\n'));
        assert!(!out.contains('"'));
        assert!(out.contains("Line 1 Line 2 with"));
    }
}
