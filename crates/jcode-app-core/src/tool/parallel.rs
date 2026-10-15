//! Shared helpers for the Parallel-backed web tools (`websearch`, `webfetch`).
//!
//! The API key comes from the `PARALLEL_API_KEY` process environment variable
//! only. There is no config-file section for it, so changing the key requires
//! a daemon restart (process env is fixed at spawn).

/// Parallel Search endpoint (`POST`).
pub const PARALLEL_SEARCH_URL: &str = "https://api.parallel.ai/v1/search";
/// Parallel Extract endpoint (`POST`).
pub const PARALLEL_EXTRACT_URL: &str = "https://api.parallel.ai/v1/extract";
/// Per-request timeout for Parallel API calls.
pub const PARALLEL_REQUEST_TIMEOUT_SECS: u64 = 60;
/// Ceiling on excerpt characters requested from Parallel Search.
pub const PARALLEL_MAX_CHARS_TOTAL: usize = 40_000;

/// Read the Parallel API key from the process environment.
///
/// Returns an error naming `PARALLEL_API_KEY` when the variable is unset or
/// holds only whitespace.
pub fn parallel_api_key() -> anyhow::Result<String> {
    if let Ok(raw) = std::env::var("PARALLEL_API_KEY")
        && !raw.trim().is_empty()
    {
        return Ok(raw.trim().to_string());
    }
    Err(anyhow::anyhow!(
        "PARALLEL_API_KEY is not set or is empty. Set PARALLEL_API_KEY in the \
         environment (e.g. `export PARALLEL_API_KEY=...`) and restart the daemon."
    ))
}

/// Extract the API's own error message from a `{type:"error",error:{message}}`
/// body. Returns `None` when the body does not match that shape.
pub fn parse_api_error_message(body: &str) -> Option<String> {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return None,
    };
    if value.get("type").and_then(|kind| kind.as_str()) != Some("error") {
        return None;
    }
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .map(|text| text.to_string())
}

/// Render an API failure for any non-success HTTP status: prefer the API's own
/// error message, fall back to the raw body. `ref_id` is intentionally
/// dropped — an opaque support token with no meaning to the model.
pub fn render_api_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    match parse_api_error_message(body) {
        Some(message) => anyhow::anyhow!("{message} (HTTP {status})"),
        None => anyhow::anyhow!("{body} (HTTP {status})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_key(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(value) => crate::env::set_var("PARALLEL_API_KEY", value),
            None => crate::env::remove_var("PARALLEL_API_KEY"),
        }
    }

    #[test]
    fn returns_key_when_set() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::set_var("PARALLEL_API_KEY", "test-key-123");
        let result = parallel_api_key();
        restore_key(prev);
        assert_eq!(result.unwrap(), "test-key-123");
    }

    #[test]
    fn errors_when_unset_naming_variable() {
        let _guard = crate::storage::lock_test_env();
        let prev = std::env::var_os("PARALLEL_API_KEY");
        crate::env::remove_var("PARALLEL_API_KEY");
        let err = parallel_api_key().unwrap_err().to_string();
        restore_key(prev);
        assert!(
            err.contains("PARALLEL_API_KEY"),
            "error should name the variable: {err}"
        );
    }

    #[test]
    fn errors_when_empty_or_whitespace() {
        for value in ["", "   ", "\t\n "] {
            let _guard = crate::storage::lock_test_env();
            let prev = std::env::var_os("PARALLEL_API_KEY");
            crate::env::set_var("PARALLEL_API_KEY", value);
            let err = parallel_api_key().unwrap_err().to_string();
            restore_key(prev);
            assert!(
                err.contains("PARALLEL_API_KEY"),
                "error should name the variable for {value:?}: {err}"
            );
        }
    }

    #[test]
    fn parses_error_shape_and_rejects_other_bodies() {
        let body = r#"{"type":"error","error":{"ref_id":"abc","message":"bad key"}}"#;
        assert_eq!(parse_api_error_message(body).as_deref(), Some("bad key"));
        assert_eq!(parse_api_error_message("not json"), None);
        assert_eq!(parse_api_error_message(r#"{"type":"ok"}"#), None);
    }

    #[test]
    fn render_api_error_prefers_parsed_message() {
        let status = reqwest::StatusCode::UNAUTHORIZED;
        let body = r#"{"type":"error","error":{"ref_id":"x","message":"invalid api key"}}"#;
        let err = render_api_error(status, body).to_string();
        assert!(err.contains("invalid api key"), "{err}");
        assert!(err.contains("401"), "{err}");
        let raw = render_api_error(status, "<html>oops</html>").to_string();
        assert!(raw.contains("<html>oops</html>"), "{raw}");
    }
}
