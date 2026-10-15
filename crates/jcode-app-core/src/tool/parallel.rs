//! Shared helpers for the Parallel-backed web tools (`websearch`, `webfetch`).
//!
//! The API key comes from the `PARALLEL_API_KEY` process environment variable
//! only. There is no config-file section for it, so changing the key requires
//! a daemon restart (process env is fixed at spawn).
//!
//! The API origin is overridable through `JCODE_PARALLEL_API_BASE`, or through
//! the vendor's own `PARALLEL_BASE_URL`, for egress proxies, caching gateways
//! and local recorders. The value is an origin, optionally with a path prefix:
//! `parallel_search_url` / `parallel_extract_url` append `/v1/search` and
//! `/v1/extract`. Nothing is sent anywhere but `api.parallel.ai` unless one of
//! those variables is set, and `PARALLEL_API_KEY` travels to whatever host the
//! override names. Like the key, the value is read per request, but a daemon
//! only ever sees the environment it was spawned with.

/// Default Parallel API origin. `parallel_search_url` / `parallel_extract_url`
/// append the `/v1/<resource>` paths, so an override names an origin (optionally
/// with a path prefix), never a versioned base.
const DEFAULT_PARALLEL_BASE_URL: &str = "https://api.parallel.ai";
/// Origin overrides, in precedence order: this repo's convention first, then the
/// vendor's own knob (Parallel's Python SDK reads `PARALLEL_BASE_URL`).
const PARALLEL_BASE_URL_VARS: [&str; 2] = ["JCODE_PARALLEL_API_BASE", "PARALLEL_BASE_URL"];
/// Per-request timeout for Parallel API calls.
pub const PARALLEL_REQUEST_TIMEOUT_SECS: u64 = 60;
/// Ceiling on excerpt characters requested from Parallel Search.
pub const PARALLEL_MAX_CHARS_TOTAL: usize = 40_000;

/// Resolve the Parallel API origin.
///
/// The first non-blank, valid override wins; blank values are skipped and
/// invalid values are skipped with a warning, so a typo never silently
/// redirects a live request to an unrelated host. Read per request, mirroring
/// `provider::openai::resolve_api_base`, so no cache needs invalidating.
fn parallel_base_url() -> String {
    for var in PARALLEL_BASE_URL_VARS {
        let Ok(raw) = std::env::var(var) else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match crate::provider_catalog::normalize_api_base(trimmed) {
            Some(base) => {
                if base.ends_with("/v1") {
                    crate::logging::warn(&format!(
                        "{var} '{}' already ends in /v1; jcode appends /v1/search and \
                         /v1/extract itself, so requests will go to {base}/v1/...",
                        trimmed
                    ));
                }
                return base;
            }
            None => crate::logging::warn(&format!(
                "Ignoring invalid {var} '{}'; expected an absolute http(s):// origin. \
                 Plain http is accepted only for loopback, private, link-local, .local \
                 and .lan hosts, because the Parallel API key is sent to this host.",
                trimmed
            )),
        }
    }
    DEFAULT_PARALLEL_BASE_URL.to_string()
}

/// Parallel Search endpoint (`POST`).
pub fn parallel_search_url() -> String {
    format!("{}/v1/search", parallel_base_url())
}

/// Parallel Extract endpoint (`POST`).
pub fn parallel_extract_url() -> String {
    format!("{}/v1/extract", parallel_base_url())
}

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

    /// Run `f` with only the given base-URL overrides in place: both variables
    /// are cleared first so the "no override" case is deterministic, and every
    /// value is restored afterwards. Holds the process-env lock, because env is
    /// process-global.
    fn with_base_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        let _guard = crate::storage::lock_test_env();
        let prev: Vec<(String, Option<std::ffi::OsString>)> = PARALLEL_BASE_URL_VARS
            .iter()
            .map(|var| ((*var).to_string(), std::env::var_os(var)))
            .collect();
        for var in PARALLEL_BASE_URL_VARS {
            crate::env::remove_var(var);
        }
        for (var, value) in vars {
            match value {
                Some(value) => crate::env::set_var(var, value),
                None => crate::env::remove_var(var),
            }
        }
        let result = f();
        for (var, value) in prev {
            match value {
                Some(value) => crate::env::set_var(&var, value),
                None => crate::env::remove_var(&var),
            }
        }
        result
    }

    #[test]
    fn web_urls_default_to_the_parallel_origin() {
        with_base_env(&[], || {
            assert_eq!(parallel_search_url(), "https://api.parallel.ai/v1/search");
            assert_eq!(parallel_extract_url(), "https://api.parallel.ai/v1/extract");
        });
    }

    #[test]
    fn override_replaces_the_origin_and_keeps_the_version_path() {
        with_base_env(
            &[("JCODE_PARALLEL_API_BASE", Some("https://proxy.example.com"))],
            || {
                assert_eq!(parallel_search_url(), "https://proxy.example.com/v1/search");
                assert_eq!(
                    parallel_extract_url(),
                    "https://proxy.example.com/v1/extract"
                );
            },
        );
    }

    #[test]
    fn override_keeps_a_path_prefix_and_strips_a_trailing_slash() {
        with_base_env(
            &[(
                "JCODE_PARALLEL_API_BASE",
                Some("https://proxy.example.com/parallel/"),
            )],
            || {
                assert_eq!(
                    parallel_search_url(),
                    "https://proxy.example.com/parallel/v1/search"
                );
            },
        );
    }

    #[test]
    fn vendor_alias_is_honoured_and_the_jcode_name_wins() {
        with_base_env(
            &[("PARALLEL_BASE_URL", Some("https://vendor.example.com"))],
            || {
                assert_eq!(
                    parallel_search_url(),
                    "https://vendor.example.com/v1/search"
                );
            },
        );
        with_base_env(
            &[
                ("PARALLEL_BASE_URL", Some("https://vendor.example.com")),
                ("JCODE_PARALLEL_API_BASE", Some("https://jcode.example.com")),
            ],
            || {
                assert_eq!(parallel_search_url(), "https://jcode.example.com/v1/search");
            },
        );
    }

    #[test]
    fn blank_override_is_skipped_not_fatal() {
        with_base_env(&[("JCODE_PARALLEL_API_BASE", Some(""))], || {
            assert_eq!(parallel_search_url(), "https://api.parallel.ai/v1/search");
        });
        with_base_env(
            &[
                ("JCODE_PARALLEL_API_BASE", Some("   ")),
                ("PARALLEL_BASE_URL", Some("https://vendor.example.com")),
            ],
            || {
                assert_eq!(
                    parallel_search_url(),
                    "https://vendor.example.com/v1/search"
                );
            },
        );
    }

    #[test]
    fn invalid_override_falls_back_to_the_default() {
        for invalid in ["notaurl", "ftp://x", "http://public.example.com"] {
            with_base_env(&[("JCODE_PARALLEL_API_BASE", Some(invalid))], || {
                assert_eq!(
                    parallel_search_url(),
                    "https://api.parallel.ai/v1/search",
                    "invalid override {invalid:?} should be ignored"
                );
            });
        }
    }

    #[test]
    fn loopback_plain_http_override_is_allowed() {
        with_base_env(
            &[("JCODE_PARALLEL_API_BASE", Some("http://127.0.0.1:8080"))],
            || {
                assert_eq!(parallel_search_url(), "http://127.0.0.1:8080/v1/search");
            },
        );
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
