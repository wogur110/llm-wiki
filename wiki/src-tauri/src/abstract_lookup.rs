//! Fetch a paper abstract from public scholarly-metadata APIs.
//!
//! Used by the Zotero importer so we never have to send the full PDF to
//! Gemini — the metadata-only flow is cheaper, faster, and avoids HTTP 400
//! "PDF too large / unsupported" errors entirely.
//!
//! Lookup order:
//!   1. **Crossref**         (`https://api.crossref.org/works/{doi}`)
//!   2. **Semantic Scholar** (`https://api.semanticscholar.org/graph/v1/paper/DOI:{doi}`)
//!   3. **OpenAlex**         (`https://api.openalex.org/works/https://doi.org/{doi}`)
//!
//! All three are free, key-less APIs.  Any single backend failure is
//! silent — callers receive `None` only when **every** backend returned no
//! usable abstract.
//!
//! No external commands are exposed; the module is consumed internally by
//! `pdf_import::import_zotero_item_and_organize`.

use reqwest::Client;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Polite User-Agent per Crossref's etiquette guidelines.  The mailto address
/// is a no-op placeholder — Crossref only uses it to contact heavy users.
const USER_AGENT: &str =
    "LLM-Wiki/0.1 (https://github.com/wogur110/llm-wiki; mailto:noreply@llm-wiki.local)";

fn build_client() -> Client {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .expect("abstract_lookup: failed to build HTTP client")
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Try every DOI-based backend in order; return the first non-empty abstract.
pub async fn fetch_abstract_by_doi(doi: &str) -> Option<String> {
    let doi = doi.trim();
    if doi.is_empty() {
        return None;
    }
    let client = build_client();

    if let Some(a) = fetch_crossref(&client, doi).await {
        return Some(a);
    }
    if let Some(a) = fetch_semantic_scholar_by_doi(&client, doi).await {
        return Some(a);
    }
    if let Some(a) = fetch_openalex_by_doi(&client, doi).await {
        return Some(a);
    }
    None
}

/// Title-based fallback when the Zotero item has no DOI.
///
/// Uses Semantic Scholar's `/paper/search` endpoint — Crossref's title search
/// is too noisy for unique matching.
pub async fn fetch_abstract_by_title(title: &str) -> Option<String> {
    let t = title.trim();
    if t.is_empty() {
        return None;
    }
    let client = build_client();
    fetch_semantic_scholar_by_title(&client, t).await
}

// ── Backend: Crossref ─────────────────────────────────────────────────────────

async fn fetch_crossref(client: &Client, doi: &str) -> Option<String> {
    let url = format!("https://api.crossref.org/works/{}", url_encode_path(doi));
    let json: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let raw = json.get("message")?.get("abstract")?.as_str()?;
    let cleaned = normalise_whitespace(&strip_html_tags(raw));
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

// ── Backend: Semantic Scholar ─────────────────────────────────────────────────

async fn fetch_semantic_scholar_by_doi(client: &Client, doi: &str) -> Option<String> {
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/DOI:{}?fields=abstract",
        url_encode_path(doi)
    );
    let json: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let raw = json.get("abstract")?.as_str()?;
    let cleaned = normalise_whitespace(raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

async fn fetch_semantic_scholar_by_title(client: &Client, title: &str) -> Option<String> {
    let url = "https://api.semanticscholar.org/graph/v1/paper/search";
    let json: serde_json::Value = client
        .get(url)
        .query(&[
            ("query", title),
            ("limit", "1"),
            ("fields", "abstract,title"),
        ])
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let raw = json
        .get("data")?
        .as_array()?
        .first()?
        .get("abstract")?
        .as_str()?;
    let cleaned = normalise_whitespace(raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

// ── Backend: OpenAlex ─────────────────────────────────────────────────────────

async fn fetch_openalex_by_doi(client: &Client, doi: &str) -> Option<String> {
    let url = format!(
        "https://api.openalex.org/works/https://doi.org/{}",
        url_encode_path(doi)
    );
    let json: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let inverted = json.get("abstract_inverted_index")?.as_object()?;
    let cleaned = normalise_whitespace(&reconstruct_inverted_index(inverted));
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

// ── Text helpers ─────────────────────────────────────────────────────────────

/// OpenAlex stores abstracts as `{word: [positions]}` so the text doesn't
/// trip Elasticsearch copyright filters.  Reconstruct word order from the
/// position arrays.
pub(crate) fn reconstruct_inverted_index(
    inverted: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut positions: Vec<(usize, &str)> = Vec::new();
    for (word, locs) in inverted {
        if let Some(arr) = locs.as_array() {
            for pos in arr {
                if let Some(p) = pos.as_u64() {
                    positions.push((p as usize, word.as_str()));
                }
            }
        }
    }
    positions.sort_by_key(|(p, _)| *p);
    positions
        .into_iter()
        .map(|(_, w)| w)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip JATS / HTML tags without bringing in a full HTML parser.  Anything
/// between `<` and `>` is removed; the angle brackets are replaced by a
/// single space so adjacent words do not get glued together.
pub(crate) fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Collapse runs of whitespace into single spaces and trim.
pub(crate) fn normalise_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Percent-encode a string for use inside a URL path segment.
///
/// Keeps the characters that are safe inside a DOI path (`A-Za-z0-9-._~/:`)
/// and percent-encodes everything else.  Good enough for DOIs which never
/// contain spaces or fragments.
fn url_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_html_tags_removes_jats_paragraph() {
        let jats = "<jats:p>Hello <jats:italic>world</jats:italic>.</jats:p>";
        let cleaned = normalise_whitespace(&strip_html_tags(jats));
        assert_eq!(cleaned, "Hello world .");
    }

    #[test]
    fn strip_html_tags_handles_text_without_tags() {
        let plain = "plain abstract.";
        assert_eq!(strip_html_tags(plain), plain);
    }

    #[test]
    fn normalise_whitespace_collapses_runs() {
        assert_eq!(
            normalise_whitespace("  multiple\n\t  spaces  here  "),
            "multiple spaces here"
        );
    }

    #[test]
    fn url_encode_path_encodes_special_chars() {
        assert_eq!(url_encode_path("10.1/abc def"), "10.1/abc%20def");
        assert_eq!(url_encode_path("plain"), "plain");
        assert_eq!(url_encode_path("10.48550/arXiv.2106.09685"), "10.48550/arXiv.2106.09685");
    }

    #[test]
    fn reconstruct_inverted_index_orders_by_position() {
        let m = json!({
            "world": [1],
            "Hello": [0],
            "!": [2],
        });
        let map = m.as_object().unwrap();
        assert_eq!(reconstruct_inverted_index(map), "Hello world !");
    }

    #[test]
    fn reconstruct_inverted_index_handles_repeated_words() {
        let m = json!({
            "the": [0, 2],
            "cat": [1],
            "ran": [3],
        });
        let map = m.as_object().unwrap();
        assert_eq!(reconstruct_inverted_index(map), "the cat the ran");
    }

    /// `fetch_abstract_by_doi` must short-circuit to `None` for empty input
    /// without making any HTTP request.
    #[tokio::test]
    async fn fetch_abstract_by_doi_returns_none_for_empty_doi() {
        assert!(fetch_abstract_by_doi("").await.is_none());
        assert!(fetch_abstract_by_doi("   ").await.is_none());
    }

    #[tokio::test]
    async fn fetch_abstract_by_title_returns_none_for_empty_title() {
        assert!(fetch_abstract_by_title("").await.is_none());
    }
}
