use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

/// Cap on the text handed back to the model. Full pages routinely exceed 150 KB
/// (~40k tokens) which is rarely worth the context budget.
const MAX_OUTPUT_CHARS: usize = 40_000;

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            client: crate::provider::shared_http_client(),
        }
    }

    /// Execute against an explicit endpoint. Production passes
    /// `PARALLEL_EXTRACT_URL`; tests pass a mock server URL.
    async fn execute_with_endpoint(
        &self,
        input: Value,
        _ctx: ToolContext,
        endpoint: &str,
    ) -> Result<ToolOutput> {
        let params: WebFetchInput = serde_json::from_value(input)?;
        let validated = validate_fetch_input(&params)?;
        let api_key = super::parallel::parallel_api_key()?;
        let body = build_extract_body(
            &validated.urls,
            validated.objective,
            &validated.queries,
            validated.full_content,
        );

        // TODO(later): thread session_id + client_model.
        let response = self
            .client
            .post(endpoint)
            .header("x-api-key", api_key)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (compatible; JCode/1.0)",
            )
            .timeout(Duration::from_secs(
                super::parallel::PARALLEL_REQUEST_TIMEOUT_SECS,
            ))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_body =
                jcode_provider_core::http_error_body(response, "parallel extract").await;
            return Err(super::parallel::render_api_error(status, &error_body));
        }

        let parsed: ParallelExtractResponse = response.json().await?;
        let rendered = render_extract_output(&parsed);
        let full_len = rendered.len();
        let (output, output_truncated) = truncate_output(rendered);

        let note = if output_truncated {
            format!(
                "\n\n(output truncated to {MAX_OUTPUT_CHARS} of {full_len} chars; \
                 fetch a more specific URL or anchor for the rest)"
            )
        } else {
            String::new()
        };

        Ok(ToolOutput::new(format!("{output}{note}")))
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    fn description(&self) -> &str {
        "Fetch page content using Parallel (public URLs only)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["urls"],
            "properties": {
                "intent": super::intent_schema_property(),
                "urls": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "maxItems": 20,
                    "description": "Public page URLs to fetch (up to 20; not localhost, private, or login-gated)."
                },
                "objective": {
                    "type": "string",
                    "description": "What to focus on when extracting content."
                },
                "search_queries": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Keywords used together with objective to focus excerpts."
                },
                "full_content": {
                    "type": "boolean",
                    "description": "When true, request full page content instead of excerpts."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        self.execute_with_endpoint(input, ctx, super::parallel::PARALLEL_EXTRACT_URL)
            .await
    }
}

#[derive(Debug, Deserialize)]
struct WebFetchInput {
    urls: Vec<String>,
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    search_queries: Option<Vec<String>>,
    #[serde(default)]
    full_content: Option<bool>,
}

#[derive(Debug)]
struct ValidatedFetchInput {
    urls: Vec<String>,
    objective: Option<String>,
    queries: Vec<String>,
    full_content: bool,
}

/// Require 1..=20 `http(s)` URLs; blank objectives/queries are dropped, and
/// `full_content` defaults to false.
fn validate_fetch_input(input: &WebFetchInput) -> Result<ValidatedFetchInput> {
    if input.urls.is_empty() {
        return Err(anyhow::anyhow!("urls must contain at least one URL"));
    }
    if input.urls.len() > 20 {
        return Err(anyhow::anyhow!(
            "urls must contain at most 20 URLs (got {})",
            input.urls.len()
        ));
    }
    for url in &input.urls {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(anyhow::anyhow!("URL must start with http:// or https://"));
        }
    }
    let objective = match &input.objective {
        Some(objective) if !objective.trim().is_empty() => Some(objective.trim().to_string()),
        _ => None,
    };
    let mut queries = Vec::new();
    if let Some(raw) = &input.search_queries {
        for query in raw {
            if !query.trim().is_empty() {
                queries.push(query.trim().to_string());
            }
        }
    }
    let full_content = matches!(input.full_content, Some(true));
    Ok(ValidatedFetchInput {
        urls: input.urls.clone(),
        objective,
        queries,
        full_content,
    })
}

/// Build the Parallel Extract request body. Optional fields are omitted when
/// absent or blank; `full_content` is only sent when true (the API default is
/// excerpts-only).
fn build_extract_body(
    urls: &[String],
    objective: Option<String>,
    queries: &[String],
    full_content: bool,
) -> Value {
    let mut body = json!({"urls": urls});
    if let Some(objective) = objective {
        body["objective"] = json!(objective);
    }
    if !queries.is_empty() {
        body["search_queries"] = json!(queries);
    }
    if full_content {
        body["advanced_settings"] = json!({"full_content": true});
    }
    body
}

/// Render results in response order, then error entries inline: successes are
/// kept even when some URLs fail. Only a non-success HTTP status fails the
/// whole call.
fn render_extract_output(response: &ParallelExtractResponse) -> String {
    let mut output = String::new();
    for result in &response.results {
        let title = match &result.title {
            Some(title) if !title.trim().is_empty() => title.trim().to_string(),
            _ => result.url.clone(),
        };
        let content = match &result.full_content {
            Some(content) if !content.trim().is_empty() => content.clone(),
            _ => result.excerpts.join("\n\n"),
        };
        output.push_str(&format!("## {title}\n{}\n\n{content}\n\n", result.url));
    }
    for error in &response.errors {
        let status = match error.http_status_code {
            Some(code) => code.to_string(),
            None => "unknown".to_string(),
        };
        let content = match &error.content {
            Some(content) if !content.trim().is_empty() => content.clone(),
            _ => error.error_type.clone(),
        };
        output.push_str(&format!(
            "## {} (failed: {}, HTTP {status})\n{content}\n\n",
            error.url, error.error_type
        ));
    }
    output
}

/// Truncate at a char boundary, preferring to cut at the last newline so the tail
/// is not a half-formed line.
pub(super) fn truncate_output(output: String) -> (String, bool) {
    if output.len() <= MAX_OUTPUT_CHARS {
        return (output, false);
    }
    let mut cut = MAX_OUTPUT_CHARS;
    while cut > 0 && !output.is_char_boundary(cut) {
        cut -= 1;
    }
    let slice = &output[..cut];
    let cut = match slice.rfind('\n') {
        Some(nl) if nl > MAX_OUTPUT_CHARS / 2 => nl,
        _ => cut,
    };
    (output[..cut].to_string(), true)
}

// Response shapes mirror the Parallel Extract API contract (`errors` is
// required but may be empty; `http_status_code` and `content` are nullable).
// `extract_id`, `session_id`, and `publish_date` are parsed but unused in v1.
#[allow(dead_code)]
#[derive(Deserialize)]
struct ParallelExtractResponse {
    #[serde(default)]
    extract_id: String,
    #[serde(default)]
    results: Vec<ParallelExtractResult>,
    #[serde(default)]
    errors: Vec<ExtractErrorEntry>,
    #[serde(default)]
    session_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct ParallelExtractResult {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    excerpts: Vec<String>,
    #[serde(default)]
    full_content: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct ExtractErrorEntry {
    url: String,
    error_type: String,
    #[serde(default)]
    http_status_code: Option<i64>,
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch_input(
        urls: Vec<&str>,
        objective: Option<&str>,
        search_queries: Option<Vec<&str>>,
        full_content: Option<bool>,
    ) -> WebFetchInput {
        WebFetchInput {
            urls: urls.into_iter().map(|s| s.to_string()).collect(),
            objective: objective.map(|s| s.to_string()),
            search_queries: search_queries
                .map(|queries| queries.into_iter().map(|s| s.to_string()).collect()),
            full_content,
        }
    }

    fn fetch_ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_string(),
            message_id: "test".to_string(),
            tool_call_id: "test".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: super::super::ToolExecutionMode::Direct,
        }
    }

    fn restore_parallel_key(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(value) => crate::env::set_var("PARALLEL_API_KEY", value),
            None => crate::env::remove_var("PARALLEL_API_KEY"),
        }
    }

    #[test]
    fn rejects_empty_or_oversized_url_lists() {
        let err = validate_fetch_input(&fetch_input(vec![], None, None, None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("urls"), "unexpected error: {err}");
        let many: Vec<String> = (0..21)
            .map(|i| format!("https://example.com/{i}"))
            .collect();
        let err = validate_fetch_input(&WebFetchInput {
            urls: many,
            objective: None,
            search_queries: None,
            full_content: None,
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("20"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_non_http_urls_before_network() {
        for url in ["ftp://example.com/file", "notaurl", "", "  "] {
            let err = validate_fetch_input(&fetch_input(vec![url], None, None, None))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("http://") || err.contains("URL"),
                "unexpected error for {url:?}: {err}"
            );
        }
    }

    #[test]
    fn renders_multi_url_sections_in_order() {
        let response: ParallelExtractResponse = serde_json::from_value(json!({
            "extract_id": "e1",
            "results": [
                {"url": "https://one.test", "title": "One", "excerpts": ["a", "b"]},
                {"url": "https://two.test", "title": null, "excerpts": ["only"]},
                {"url": "https://three.test", "title": "Three", "full_content": "FULL"}
            ],
            "errors": [],
            "session_id": "sess"
        }))
        .unwrap();
        let out = render_extract_output(&response);
        let one = out.find("https://one.test").unwrap();
        let two = out.find("https://two.test").unwrap();
        let three = out.find("https://three.test").unwrap();
        assert!(one < two && two < three, "order not preserved:\n{out}");
        assert!(out.contains("## One\nhttps://one.test\n\na\n\nb\n\n"));
        assert!(out.contains("## https://two.test\nhttps://two.test\n\nonly\n\n"));
        assert!(out.contains("## Three\nhttps://three.test\n\nFULL\n\n"));
    }

    #[test]
    fn renders_error_entries_inline_with_successes() {
        let response: ParallelExtractResponse = serde_json::from_value(json!({
            "extract_id": "e1",
            "results": [
                {"url": "https://ok.test", "title": "Ok", "excerpts": ["fine"]}
            ],
            "errors": [
                {"url": "https://bad.test", "error_type": "fetch_failed",
                 "http_status_code": 503, "content": "upstream exploded"}
            ],
            "session_id": "sess"
        }))
        .unwrap();
        let out = render_extract_output(&response);
        assert!(out.contains("## Ok\nhttps://ok.test\n\nfine\n\n"));
        assert!(out.contains(
            "## https://bad.test (failed: fetch_failed, HTTP 503)\nupstream exploded\n\n"
        ));
    }

    #[test]
    fn renders_unknown_status_when_null() {
        let response: ParallelExtractResponse = serde_json::from_value(json!({
            "extract_id": "e1",
            "results": [],
            "errors": [
                {"url": "https://bad.test", "error_type": "timeout",
                 "http_status_code": null, "content": null}
            ],
            "session_id": "sess"
        }))
        .unwrap();
        let out = render_extract_output(&response);
        assert!(out.contains("## https://bad.test (failed: timeout, HTTP unknown)\ntimeout\n\n"));
    }

    #[test]
    fn full_content_flag_controls_request_body() {
        let urls = vec!["https://example.com".to_string()];
        let full = build_extract_body(&urls, None, &[], true);
        assert_eq!(full["advanced_settings"]["full_content"], json!(true));
        let without = build_extract_body(&urls, None, &[], false);
        assert!(without.get("advanced_settings").is_none());
        let absent = build_extract_body(
            &urls,
            Some("summarize".to_string()),
            &["example domain".to_string()],
            false,
        );
        assert_eq!(absent["objective"], json!("summarize"));
        assert_eq!(absent["search_queries"], json!(["example domain"]));
        assert!(absent.get("advanced_settings").is_none());
    }

    #[test]
    fn validation_drops_blank_objective_and_queries() {
        let validated = validate_fetch_input(&fetch_input(
            vec!["https://example.com"],
            Some("   "),
            Some(vec!["  ", "real query"]),
            Some(true),
        ))
        .unwrap();
        assert_eq!(validated.objective, None);
        assert_eq!(validated.queries, vec!["real query".to_string()]);
        assert!(validated.full_content);
    }

    fn mock_extract_server(
        status: u16,
        response_body: &str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_server = std::sync::Arc::clone(&captured);
        let response_body = response_body.to_string();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut content_length = 0usize;
            {
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let trimmed = line.trim_end().to_string();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        content_length = rest.trim().parse().unwrap_or(0);
                    }
                }
                let mut buf = vec![0u8; content_length];
                let _ = reader.read_exact(&mut buf);
                *captured_server.lock().unwrap() = buf;
            }
            let response = format!(
                "HTTP/1.1 {status} mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        (format!("http://{addr}/v1/extract"), captured)
    }

    #[tokio::test]
    async fn fetch_without_key_fails_before_network() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::remove_var("PARALLEL_API_KEY");
        let tool = WebFetchTool::new();
        let result = tool
            .execute_with_endpoint(
                json!({"urls": ["https://example.com"]}),
                fetch_ctx(),
                "http://127.0.0.1:9/v1/extract",
            )
            .await;
        restore_parallel_key(prev);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("PARALLEL_API_KEY"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn fetch_posts_body_and_renders_sections() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::set_var("PARALLEL_API_KEY", "test-key");
        let (url, captured) = mock_extract_server(
            200,
            r#"{"extract_id":"e","results":[{"url":"https://one.test","title":"One","excerpts":["hi"]}],"errors":[],"session_id":"x"}"#,
        );
        let tool = WebFetchTool::new();
        let result = tool
            .execute_with_endpoint(
                json!({"urls": ["https://one.test"], "full_content": true}),
                fetch_ctx(),
                &url,
            )
            .await;
        let sent: serde_json::Value = serde_json::from_slice(&captured.lock().unwrap()).unwrap();
        restore_parallel_key(prev);
        let out = result.unwrap();
        assert_eq!(sent["advanced_settings"]["full_content"], json!(true));
        assert!(out.output.contains("## One\nhttps://one.test\n\nhi\n\n"));
    }

    #[tokio::test]
    async fn fetch_surfaces_api_error_message() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::set_var("PARALLEL_API_KEY", "bad-key");
        let (url, _captured) = mock_extract_server(
            402,
            r#"{"type":"error","error":{"ref_id":"r","message":"out of credits"}}"#,
        );
        let tool = WebFetchTool::new();
        let result = tool
            .execute_with_endpoint(json!({"urls": ["https://example.com"]}), fetch_ctx(), &url)
            .await;
        restore_parallel_key(prev);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("out of credits"), "unexpected error: {err}");
    }

    #[test]
    fn caps_output_length() {
        let long = "line of text\n".repeat(MAX_OUTPUT_CHARS);
        let (out, truncated) = truncate_output(long);
        assert!(truncated);
        assert!(out.len() <= MAX_OUTPUT_CHARS);
    }

    #[test]
    fn keeps_short_output_intact() {
        let (out, truncated) = truncate_output("hello".to_string());
        assert!(!truncated);
        assert_eq!(out, "hello");
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        // Multi-byte chars straddling the cut must not panic or corrupt output.
        let long = "é".repeat(MAX_OUTPUT_CHARS);
        let (out, truncated) = truncate_output(long);
        assert!(truncated);
        assert!(out.chars().all(|c| c == 'é'));
    }
}
