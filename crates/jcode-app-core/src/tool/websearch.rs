use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

/// Web search via the Parallel Search API.
pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: crate::provider::shared_http_client(),
        }
    }

    /// Execute against an explicit endpoint. Production passes
    /// `PARALLEL_SEARCH_URL`; tests pass a mock server URL.
    async fn execute_with_endpoint(
        &self,
        input: Value,
        _ctx: ToolContext,
        endpoint: &str,
    ) -> Result<ToolOutput> {
        let params: WebSearchInput = serde_json::from_value(input)?;
        let validated = validate_search_input(&params)?;
        let api_key = super::parallel::parallel_api_key()?;
        let body = build_search_body(
            &validated.objective,
            &validated.queries,
            validated.max_results,
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
            .timeout(std::time::Duration::from_secs(
                super::parallel::PARALLEL_REQUEST_TIMEOUT_SECS,
            ))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_body =
                jcode_provider_core::http_error_body(response, "parallel search").await;
            return Err(super::parallel::render_api_error(status, &error_body));
        }

        let parsed: ParallelSearchResponse = response.json().await?;
        let results = parse_parallel_results(parsed);
        let output =
            apply_search_output_budget(render_search_output(&validated.objective, &results));
        Ok(ToolOutput::new(output))
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }

    fn description(&self) -> &str {
        "Search the web using Parallel. Returns result titles, URLs, and excerpts."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["objective", "search_queries"],
            "properties": {
                "intent": super::intent_schema_property(),
                "objective": {
                    "type": "string",
                    "description": "What the search is trying to accomplish. Guides ranking and excerpts."
                },
                "search_queries": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": "Concrete search queries, 3-6 words each; provide 2-3 phrasings."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max results to return (clamped to 20)."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        self.execute_with_endpoint(input, ctx, super::parallel::PARALLEL_SEARCH_URL)
            .await
    }
}

#[derive(Debug, Deserialize)]
struct WebSearchInput {
    objective: String,
    search_queries: Vec<String>,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug)]
struct ValidatedSearchInput {
    objective: String,
    queries: Vec<String>,
    max_results: Option<usize>,
}

/// Reject blank objectives, filter blank queries (rejecting when none remain),
/// and clamp `max_results` into the API's 1..=20 range.
fn validate_search_input(input: &WebSearchInput) -> Result<ValidatedSearchInput> {
    if input.objective.trim().is_empty() {
        return Err(anyhow::anyhow!("objective must not be blank"));
    }
    let queries: Vec<String> = input
        .search_queries
        .iter()
        .filter(|query| !query.trim().is_empty())
        .map(|query| query.trim().to_string())
        .collect();
    if queries.is_empty() {
        return Err(anyhow::anyhow!(
            "search_queries must contain at least one non-blank query"
        ));
    }
    let max_results = input.max_results.map(|max| max.clamp(1, 20));
    Ok(ValidatedSearchInput {
        objective: input.objective.trim().to_string(),
        queries,
        max_results,
    })
}

/// Build the Parallel Search request body. `max_results` must already be
/// clamped; `advanced_settings` is omitted entirely when it is absent.
fn build_search_body(objective: &str, queries: &[String], max_results: Option<usize>) -> Value {
    let mut body = json!({
        "objective": objective,
        "search_queries": queries,
        "mode": "fast",
        "max_chars_total": super::parallel::PARALLEL_MAX_CHARS_TOTAL,
    });
    if let Some(max) = max_results {
        body["advanced_settings"] = json!({"max_results": max});
    }
    body
}

/// Map API results to title/url/snippet: excerpts joined with a blank line,
/// blank or missing titles falling back to the URL.
fn parse_parallel_results(response: ParallelSearchResponse) -> Vec<SearchResult> {
    response
        .results
        .into_iter()
        .map(|result| {
            let title = match result.title {
                Some(title) if !title.trim().is_empty() => title.trim().to_string(),
                _ => result.url.clone(),
            };
            SearchResult {
                title,
                url: result.url,
                snippet: result.excerpts.join("\n\n"),
            }
        })
        .collect()
}

fn render_search_output(objective: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("No results found for: {objective}");
    }
    let mut output = format!("Search results for: {objective}\n\n");
    for (i, result) in results.iter().enumerate() {
        output.push_str(&format!(
            "{}. **{}**\n   {}\n   {}\n\n",
            i + 1,
            result.title,
            result.url,
            result.snippet
        ));
    }
    output
}

/// Cut rendered output at the shared 40k-char cap with a websearch-specific
/// truncation note.
fn apply_search_output_budget(rendered: String) -> String {
    let full_len = rendered.len();
    let (output, truncated) = super::webfetch::truncate_output(rendered);
    if truncated {
        format!(
            "{output}\n\n(output truncated to {} of {full_len} chars; narrow the search or lower max_results for the rest)",
            super::parallel::PARALLEL_MAX_CHARS_TOTAL,
        )
    } else {
        output
    }
}

// Response shapes mirror the Parallel Search API contract. `search_id`,
// `session_id`, and `publish_date` are parsed but deliberately unused in v1.
#[allow(dead_code)]
#[derive(Deserialize)]
struct ParallelSearchResponse {
    #[serde(default)]
    search_id: String,
    #[serde(default)]
    results: Vec<ParallelResult>,
    #[serde(default)]
    session_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct ParallelResult {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    excerpts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_input(
        objective: &str,
        queries: Vec<&str>,
        max_results: Option<usize>,
    ) -> WebSearchInput {
        WebSearchInput {
            objective: objective.to_string(),
            search_queries: queries.into_iter().map(|s| s.to_string()).collect(),
            max_results,
        }
    }

    fn test_ctx() -> ToolContext {
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
    fn rejects_blank_objective_before_network() {
        for objective in ["", "   ", "\t\n "] {
            let err = validate_search_input(&search_input(objective, vec!["rust"], None))
                .unwrap_err()
                .to_string();
            assert!(err.contains("objective"), "unexpected error: {err}");
        }
    }

    #[test]
    fn rejects_missing_or_blank_search_queries_before_network() {
        let err = serde_json::from_value::<WebSearchInput>(json!({"objective": "rust"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("search_queries"), "unexpected error: {err}");
        for queries in [vec![], vec!["  ", "\t"]] {
            let err = validate_search_input(&search_input("rust", queries, None))
                .unwrap_err()
                .to_string();
            assert!(err.contains("search_queries"), "unexpected error: {err}");
        }
        let validated =
            validate_search_input(&search_input("rust", vec!["  ", "tokio guide"], None)).unwrap();
        assert_eq!(validated.queries, vec!["tokio guide".to_string()]);
    }

    #[test]
    fn clamps_max_results_to_api_ceiling() {
        let over = validate_search_input(&search_input("rust", vec!["rust"], Some(999))).unwrap();
        assert_eq!(over.max_results, Some(20));
        let zero = validate_search_input(&search_input("rust", vec!["rust"], Some(0))).unwrap();
        assert_eq!(zero.max_results, Some(1));
        let kept = validate_search_input(&search_input("rust", vec!["rust"], Some(5))).unwrap();
        assert_eq!(kept.max_results, Some(5));
        let unset = validate_search_input(&search_input("rust", vec!["rust"], None)).unwrap();
        assert_eq!(unset.max_results, None);
    }

    #[test]
    fn maps_successful_payload_to_title_url_snippet() {
        let response: ParallelSearchResponse = serde_json::from_value(json!({
            "search_id": "s1",
            "results": [
                {"url": "https://one.test", "title": "One", "excerpts": ["a", "b"]},
                {"url": "https://two.test", "title": "  ", "excerpts": []},
                {"url": "https://three.test", "title": null, "excerpts": ["only"]}
            ],
            "session_id": "sess"
        }))
        .unwrap();
        let results = parse_parallel_results(response);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "One");
        assert_eq!(results[0].url, "https://one.test");
        assert_eq!(results[0].snippet, "a\n\nb");
        assert_eq!(results[1].title, "https://two.test");
        assert_eq!(results[2].title, "https://three.test");
        assert_eq!(results[2].snippet, "only");

        let rendered = render_search_output("rust objective", &results);
        assert!(rendered.starts_with("Search results for: rust objective\n\n"));
        assert!(rendered.contains("1. **One**\n   https://one.test\n   a\n\nb\n\n"));

        let empty = render_search_output("rust objective", &[]);
        assert_eq!(empty, "No results found for: rust objective");
    }

    #[test]
    fn omits_advanced_settings_when_max_results_absent() {
        let body = build_search_body("rust", &["rust".to_string()], None);
        assert_eq!(body["objective"], json!("rust"));
        assert_eq!(body["mode"], json!("fast"));
        assert_eq!(body["max_chars_total"], json!(40000));
        assert!(body.get("advanced_settings").is_none());
        let body = build_search_body("rust", &["rust".to_string()], Some(5));
        assert_eq!(body["advanced_settings"]["max_results"], json!(5));
    }

    #[test]
    fn cuts_output_over_char_cap_with_note() {
        let results = vec![SearchResult {
            title: "t".to_string(),
            url: "https://example.com".to_string(),
            snippet: "x".repeat(50_000),
        }];
        let out = apply_search_output_budget(render_search_output("rust", &results));
        assert!(out.contains("truncated"), "missing truncation note");
        assert!(out.len() <= 40_000 + 500, "unexpected length {}", out.len());
    }

    // Minimal in-process HTTP mock (mirrors the update.rs test pattern): serves
    // one canned response and captures the request body for assertions.
    fn mock_search_server(
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
        (format!("http://{addr}/v1/search"), captured)
    }

    #[tokio::test]
    async fn search_without_key_fails_before_network() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::remove_var("PARALLEL_API_KEY");
        let tool = WebSearchTool::new();
        let result = tool
            .execute_with_endpoint(
                json!({"objective": "rust", "search_queries": ["rust"]}),
                test_ctx(),
                "http://127.0.0.1:9/v1/search",
            )
            .await;
        restore_parallel_key(prev);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("PARALLEL_API_KEY"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn search_posts_body_and_renders_results() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::set_var("PARALLEL_API_KEY", "test-key");
        let (url, captured) = mock_search_server(
            200,
            r#"{"search_id":"s","results":[{"url":"https://one.test","title":"One","excerpts":["hello"]}],"session_id":"x"}"#,
        );
        let tool = WebSearchTool::new();
        let result = tool
            .execute_with_endpoint(
                json!({"objective": "rust docs", "search_queries": ["rust docs"]}),
                test_ctx(),
                &url,
            )
            .await;
        let sent: serde_json::Value = serde_json::from_slice(&captured.lock().unwrap()).unwrap();
        restore_parallel_key(prev);
        let out = result.unwrap();
        assert_eq!(sent["mode"], json!("fast"));
        assert_eq!(sent["max_chars_total"], json!(40000));
        assert!(sent.get("advanced_settings").is_none());
        assert!(out.output.starts_with("Search results for: rust docs"));
        assert!(out.output.contains("1. **One**"));
    }

    #[tokio::test]
    async fn search_surfaces_api_error_message() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::set_var("PARALLEL_API_KEY", "bad-key");
        let (url, _captured) = mock_search_server(
            401,
            r#"{"type":"error","error":{"ref_id":"r","message":"invalid api key"}}"#,
        );
        let tool = WebSearchTool::new();
        let result = tool
            .execute_with_endpoint(
                json!({"objective": "rust", "search_queries": ["rust"]}),
                test_ctx(),
                &url,
            )
            .await;
        restore_parallel_key(prev);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid api key"), "unexpected error: {err}");
    }
}
