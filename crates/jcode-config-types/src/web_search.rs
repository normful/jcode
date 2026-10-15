//! `websearch` configuration: the provider-native search knobs.
//!
//! Local search engines were removed when `websearch` moved to the Parallel
//! Search API, so this module holds only the settings that govern
//! provider-native (server-side) search.

use super::*;

/// Configuration for the `websearch` tool.
///
/// Local search runs through the Parallel Search API (key from
/// `PARALLEL_API_KEY`), so the old scraping-engine knobs (`engine`,
/// `fallback_engines`, `bing_*`, `searxng_*`) and the custom HTTP JSON
/// provider are gone. What remains configures *provider-native* search, which
/// is orthogonal: providers with server-side web search run the query
/// themselves and the built-in local tool is swapped for the hosted one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Prefer the model provider's own server-side search whenever the active
    /// provider/model supports it (Anthropic first-party API, OpenAI
    /// Responses). The local `websearch` tool is then replaced by the hosted
    /// one. Providers without native search keep the local Parallel-backed
    /// tool. Default true.
    pub prefer_native: bool,
    /// Maximum provider-native searches per request.
    /// Anthropic bills roughly $10 per 1,000 searches on API keys, so this caps
    /// spend. Anthropic only; OpenAI has no per-request cap.
    pub native_max_uses: Option<u32>,
    /// Restrict provider-native search to these domains. Mutually exclusive
    /// with `native_blocked_domains` (Anthropic rejects both).
    pub native_allowed_domains: Vec<String>,
    /// Never return results from these domains (Anthropic only).
    pub native_blocked_domains: Vec<String>,
    /// Anthropic server tool version, e.g. "web_search_20250305" (default) or
    /// "web_search_20260209". Newer versions are sent with
    /// `allowed_callers = ["direct"]` so models without programmatic tool
    /// calling are not rejected.
    pub native_anthropic_tool_version: String,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            prefer_native: true,
            native_max_uses: Some(DEFAULT_NATIVE_WEB_SEARCH_MAX_USES),
            native_allowed_domains: Vec::new(),
            native_blocked_domains: Vec::new(),
            native_anthropic_tool_version: DEFAULT_ANTHROPIC_WEB_SEARCH_TOOL.to_string(),
        }
    }
}

/// Default cap on provider-native searches per request.
pub const DEFAULT_NATIVE_WEB_SEARCH_MAX_USES: u32 = 5;
/// Default Anthropic web search server tool version.
pub const DEFAULT_ANTHROPIC_WEB_SEARCH_TOOL: &str = "web_search_20250305";

impl WebSearchConfig {
    /// True when provider-native search should be used where available.
    ///
    /// The local Parallel-backed tool is the only local backend left, so this
    /// is just the user's preference.
    pub fn native_enabled(&self) -> bool {
        self.prefer_native
    }
}
