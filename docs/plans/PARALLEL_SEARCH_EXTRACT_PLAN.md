# Plan: Parallel-backed websearch + webfetch (TDD)

> Status: Approved plan, not yet implemented. Replaces the keyless-scraper
> websearch engines (DuckDuckGo/Bing-HTML/SearXNG) and the local-regex webfetch
> extractor with Parallel's Search (`POST /v1/search`) and Extract
> (`POST /v1/extract`) APIs. No login flow, no Responses/research API, no
> fallbacks.
>
> How to read this plan: every phase is red-green. The **T step** lists test
> cases to write FIRST (behavior descriptions only) and the command that must
> FAIL before proceeding. The **S step** lists the source changes
> (file paths + line-by-line edits) that turn them green. Do not write source
> before its tests fail. Do not write test bodies from this plan beyond what
> the descriptions say — the descriptions are the spec.

## References (authoritative; the only API sources for this work)

- Search API reference: `https://docs.parallel.ai/api-reference/search/search.md`
  (request/response schemas, modes, `advanced_settings.max_results`,
  422 shape).
- Extract API reference:
  `https://docs.parallel.ai/api-reference/extract/extract.md`
  (request/response schemas, `errors[]` semantics, `full_content` default,
  422 shape).
- Supplemental: `https://docs.parallel.ai/search/search-quickstart`,
  `https://docs.parallel.ai/extract/extract-quickstart`.

## Transport (fixed for both tools)

- Raw `reqwest` POSTs from the existing shared client
  (`crate::provider::shared_http_client()`). **No new Cargo dependencies.**
  There is no Rust SDK involved in this plan.
- Header `x-api-key: <key>` (NOT `Authorization: Bearer` — that scheme is
  Responses-only per the docs).
- `Content-Type: application/json`; 60s per-request `.timeout()` (the shared
  client keeps its own 15s connect timeout).
- Keep sending the existing
  `User-Agent: Mozilla/5.0 (compatible; JCode/1.0)` override on the new POSTs
  (identifies the client; harmless to the API).

## Goals

1. `websearch` takes `{objective, search_queries[], max_results?}` and returns
   ranked results with excerpts via Parallel Search.
2. `webfetch` takes `{urls[], objective?, search_queries?, full_content?}` and
   always returns markdown via Parallel Extract.
3. One shared `[parallel]` config section + `PARALLEL_API_KEY`; no login flow.
4. No fallbacks of any kind: no old engines, no local extraction fallback.
5. All upstream Rust callers of the old input shapes updated in the same change.

## Non-goals (v1)

- No OAuth / `/login` provider / auth-store entry (simple key only).
- No `web_research` / Responses API.
- `session_id` and `client_model` are NOT sent; leave `// TODO` comments at
  both request-build sites for later threading.
- No `max_chars_total`, `fetch_policy`, or `excerpt_settings` tuning surface;
  client-side `truncate_output` (40k chars) stays the single budget control.

## Locked Decisions (do not relitigate)

- Old engines deleted, including tests, fixtures, corpus harness, old config.
- `objective` required on websearch (the API itself leaves it optional; we
  require it). `mode: "fast"` always sent.
- `max_results` optional -> `advanced_settings.max_results` only when set.
  Any provided value is clamped to 1..=20 (old ceiling; API default 10
  otherwise). A value of 0 therefore becomes 1, never an error.
- Blank-string hygiene: whitespace-only `objective` is rejected; blank entries
  inside `search_queries` are filtered out, and if none remain the input is
  rejected. Same filtering applies to webfetch's optional `search_queries`.
  (Webfetch `urls` need no blank special-case: a blank url fails the
  `http(s)` check like any other invalid url.)
- Webfetch `search_queries` included as optional; its schema description uses
  the API wording ("used together with objective to focus excerpts").
- Webfetch `full_content?: bool` -> sends `full_content: true` only when true;
  otherwise omitted (API default excerpts-only).
- Extract per-URL `errors[]` rendered inline with successes kept; only
  non-200/422 fails the whole call, propagating the API `error.message`.
- Summaries: websearch shows truncated `objective`; webfetch shows truncated
  full URL when single, else hosts of first 3 urls (16 chars each) +
  `" + N more"`.
- Key resolution: config `[parallel].api_key` -> `PARALLEL_API_KEY` env ->
  hard error. No `_env` indirection field.

## Exact output templates (normative; tests pin these)

Websearch success (header embeds the objective, same shape as today):
`Search results for: {objective}\n\n` then per result
`{i}. **{title}**\n   {url}\n   {snippet}\n\n`
where snippet is the result's excerpts joined with `\n\n` and title falls back
to the url when null/blank. Empty results: exactly
`No results found for: {objective}` (no appended help text; the old
TLS-fingerprinting guidance is deleted with the old engines).

Webfetch success, per `results` entry in order:
`## {title or url}\n{url}\n\n{full_content or excerpts-joined}\n\n`,
then per `errors` entry in order:
`## {url} (failed: {error_type}, HTTP {status})\n{content or error_type}\n\n`.
The whole concatenation then passes through the unchanged `truncate_output`
(40k chars, newline-aware cut, truncation note). The old
`Fetched {url} ({n} bytes)` header is gone: with up to 20 urls there is no
single url or byte count to report.

Tool `description()` strings (the model reads these for routing, so they must
name the backend): websearch becomes `"Search the web using Parallel."`,
webfetch becomes `"Fetch page content using Parallel."`.

## API Contract (from the references above)

- Search body: `{objective, search_queries (required, >=1),
  mode: "fast", advanced_settings?: {max_results}}`.
  Response: `{search_id, results: [{url, excerpts[] (required),
  title?/publish_date? (nullable)}], warnings?, usage?, session_id}`.
- Extract body: `{urls (required, <=20), objective?, search_queries?,
  advanced_settings?: {full_content: true}}`.
  Response: `{extract_id, results: [{url, excerpts[], title?,
  publish_date?, full_content?}], errors: [{url, error_type,
  http_status_code, content}] (required, may be empty), session_id}`.
  With `objective` -> focused excerpts; without -> whole-page markdown.
- Failure shape (both): `{type: "error", error: {ref_id, message}}` on 422.

---

## Phase 1: Shared `[parallel]` config + key helper

### Step 1T (tests first — descriptions only)

- New behavior: the default config carries an empty Parallel key (no key
  configured out of the box).
- New behavior: setting the `JCODE_PARALLEL_API_KEY` environment variable to a
  non-empty value populates the Parallel key; an empty/whitespace value leaves
  it empty.
- New behavior: the shared key-resolution helper returns the configured key
  when set, falls back to the environment variable when unset, and fails with
  a message naming both the config field and the env var when neither exists.
- Update to the existing liveness test
  (`crates/jcode-base/src/config/change_report_tests.rs`, key list around
  line 120): the `websearch.engine` entry becomes the new Parallel key entry,
  asserting it is live (re-read through `config()`, no restart).
- RUN `cargo test -p jcode-base config::change_report` — expect RED
  (compile failure: the new config field does not exist yet).

### Step 1S (source — file paths + line-by-line changes)

1. `crates/jcode-config-types/src/lib.rs`, after `WebSearchConfig`'s
   `Default` impl (ends line 1216):
   - Insert a new `ParallelConfig` struct with one field, `api_key:
     Option<String>`, plus a `Default` impl yielding `None`. Derive the same
     traits as `WebSearchConfig` (`Debug, Clone, Serialize, Deserialize`,
     `#[serde(default)]`).
2. `crates/jcode-base/src/config.rs`, lines 486-488 (the `websearch` field
   with doc comment `/// Web search tool configuration`):
   - Insert a sibling field `pub parallel: ParallelConfig` with doc comment
     `/// Parallel API configuration (websearch + webfetch)`, keeping field
     order alphabetical-ish as today (`parallel` slots next to the provider
     fields; exact spot is cosmetic).
   - Add the import if `ParallelConfig` is not already re-exported alongside
     `WebSearchConfig`.
3. `crates/jcode-base/src/config/default_file.rs`, the `[websearch]` block
   (lines ~296-315):
   - Replace the whole block with a `[parallel]` block documenting the single
     key: commented `# api_key = ""` plus a note that `PARALLEL_API_KEY`
     takes precedence when set. (Old-engine deletion itself happens in
     Phase 5; this step only ADDS the new section so Phase 1 tests pass.
     Leaving the stale `[websearch]` block in place until Phase 5 is
     intentional.)
4. `crates/jcode-base/src/config/env_overrides.rs`, after the
   `JCODE_SEARXNG_URL` block (lines 531-535):
   - Append a `JCODE_PARALLEL_API_KEY` block following the exact shape of the
     `JCODE_BING_API_KEY` block (lines 516-520): read env, ignore when
     missing or whitespace-only, otherwise store into
     `self.parallel.api_key`.
5. `crates/jcode-app-core/src/tool/mod.rs`, module list (the `mod open;`
   line, before `mod patch;`):
   - Insert `mod parallel;` in alphabetical position.
6. New file `crates/jcode-app-core/src/tool/parallel.rs` containing:
   - `pub const PARALLEL_SEARCH_URL: &str =
     "https://api.parallel.ai/v1/search";`
   - `pub const PARALLEL_EXTRACT_URL: &str =
     "https://api.parallel.ai/v1/extract";`
   - `pub const PARALLEL_REQUEST_TIMEOUT_SECS: u64 = 60;`
   - `pub fn parallel_api_key() -> anyhow::Result<String>` implementing the
     config-then-env-then-error chain from the Locked Decisions, with the
     error message naming `` `parallel.api_key` `` and `PARALLEL_API_KEY`.
   - `pub fn render_api_error(status, message) -> anyhow::Error`
     formatting `{message} (HTTP {status})` for the 422 shape, so both tools
     share one error rendering. The API's `ref_id` is intentionally dropped,
     not passed in at all: it is an opaque support-ticket token with no
     meaning to the model and only spends context.
- RUN `cargo test -p jcode-base config` then `cargo check -p jcode-app-core`
  — expect GREEN.

---

## Phase 2: websearch rewrite

### Step 2T (tests first — descriptions only; all in the existing
`#[cfg(test)] mod tests` of `websearch.rs`, currently lines 646-837)

- New behavior: an input with an empty or whitespace-only `objective` is
  rejected with a validation error before any network call.
- New behavior: an input with a missing or empty `search_queries` array, or
  one containing only blank strings, is rejected with a validation error
  before any network call.
- New behavior: a `max_results` value above the ceiling is clamped down to
  the ceiling instead of being sent as-is; a value of 0 becomes 1.
- New behavior: a successful API payload maps each entry to title/url/snippet
  per the Exact output templates above (excerpts joined, title fallback).
- New behavior: a 422 API failure surfaces the API's own error message to the
  caller (not a generic status string).
- New behavior: attempting a search with no key configured fails with the
  configuration error from Phase 1, making no network call.
- New behavior: when `max_results` is absent, the outgoing request body
  contains no `advanced_settings` key at all; the body always contains
  `mode: "fast"`.
- Deletions to apply to the old test module in the same edit: every
  `parses_bing_*`, `parses_ddg_*`, `parses_searxng_*`,
  `searxng_results_respect_limit`, `detects_*`, `real_captured_*`,
  `real_results_are_not_flagged_*`, and `websearch_engine_*` test, plus the
  `testdata/ddg_anomaly.html` / `testdata/ddg_results.html` fixtures they
  include (delete the two fixture files in this step too).
- RUN `cargo test -p jcode-app-core tool::websearch` — expect RED
  (compile failures: the new input fields and parse helpers do not exist;
  deleted tests reference deleted items).

### Step 2S (source — file paths + line-by-line changes, all in
`crates/jcode-app-core/src/tool/websearch.rs`)

1. Line 2 (`use crate::config::WebSearchEngine;`): delete the line.
2. Lines 22-30 (`struct WebSearchInput`): replace the four fields with
   `objective: String`, `search_queries: Vec<String>`, and
   `#[serde(default)] max_results: Option<usize>`.
3. Lines 32-37 (`struct SearchResult`): keep unchanged.
4. Lines 40-44 (`struct BingSearchOptions`): delete the whole struct.
5. `description()` (line ~52): change the string to
   `"Search the web using Parallel."`.
6. Lines 56-82 (`parameters_schema`): `required` becomes
   `[objective, search_queries]`, with an `objective` string property, a
   `search_queries` string-array property (`minItems: 1`, description
   carrying the "3-6 words each, provide 2-3" guidance), and an optional
   `max_results` integer property. Delete `num_results`/`engine`/
   `bing_market`. Keep the `intent` property line as-is.
7. Lines 84-158 (`execute`): replace the whole body. New body order:
   deserialize -> validate (`objective` non-blank; filter blank queries, reject
   when none remain; clamp `max_results` with `.clamp(1, 20)`) -> resolve key
   via `super::parallel::parallel_api_key()` -> build body (`objective`,
   `search_queries`, `mode: "fast"`, plus `advanced_settings.max_results`
   only when set; leave a `// TODO(later): thread session_id + client_model`
   comment at the build site) -> `POST PARALLEL_SEARCH_URL` with `x-api-key`
   header, the JCode UA override, and 60s timeout -> 422 shape goes through
   `render_api_error`, other non-success statuses propagate as errors -> map
   results per the Exact output templates -> header
   `Search results for: {objective}`, empty case exactly
   `No results found for: {objective}`. Delete the engine loop, the
   `allow_bing_api` logic, and the old TLS-fingerprinting help text.
8. Lines 160-384 (the second `impl WebSearchTool` block): delete the whole
   block (`search_with_engine`, `search_duckduckgo`, `search_bing`,
   `search_bing_api`, `search_bing_html`, `search_searxng`).
9. Lines 386-404 (`parse_searxng_results` + doc comment): delete; add
   `parse_parallel_results(response: ParallelSearchResponse) ->
   Vec<SearchResult>` implementing the excerpt-join + title-fallback mapping.
10. Lines 406-455 (`mod search_regex`): delete the whole module.
11. Lines 457-489 (`SearxngResponse`/`SearxngResult`/`BingApiResponse`/
    `BingWebPages`/`BingWebPage`): delete all five structs; add
    `ParallelSearchResponse { search_id, results: Vec<ParallelResult>,
    session_id }` and `ParallelResult { url, title: Option<String>,
    publish_date: Option<String>, excerpts: Vec<String> }` (all
    `Deserialize`, `#[serde(default)]` where the API marks nullable).
    `publish_date` is parsed but deliberately not rendered anywhere in v1.
12. Lines 491-644 (old `parse_*`/helper fns): delete; keep only the new
    `parse_parallel_results` plus a `build_search_body` pure fn (inputs:
    objective, queries, clamped max; output: the JSON body) so the
    no-`advanced_settings`-when-absent behavior is unit-testable without
    networking.
- RUN `cargo test -p jcode-app-core tool::websearch` — expect GREEN.

---

## Phase 3: webfetch rewrite

### Step 3T (tests first — descriptions only; in the `mod tests` of
`webfetch.rs`, currently lines 458-539)

- New behavior: an input with an empty `urls` array is rejected.
- New behavior: an input with more than 20 urls is rejected.
- New behavior: an input containing a non-`http(s)` url is rejected with a
  validation error before any network call.
- New behavior: a successful multi-url payload renders one markdown section
  per result following the Exact output templates (title-or-url heading, url
  line, content), preserving response order.
- New behavior: entries in the API `errors` array render inline as failure
  blocks naming url, error type, and status, while successful entries are
  still rendered (partial success is preserved, not discarded).
- New behavior: when the optional full-content flag is true, the outgoing
  body asks the API for full content; when absent or false, the body carries
  no full-content request.
- New behavior: a 422 API failure surfaces the API's own error message.
- New behavior: attempting a fetch with no key configured fails with the
  configuration error, making no network call.
- Preserved behavior: outputs longer than the 40k-char cap are still cut at a
  newline-aware char boundary with the truncation note appended.
- Deletions to apply to the old test module in the same edit, with reasons:
  `strips_non_prose_elements`, `keeps_article_header_and_footer_content`,
  `strips_html_comments`, and `does_not_leak_attributes_containing_angle_brackets`
  (no local HTML stripping remains); `drops_empty_links_and_overlong_targets`
  (no `render_link` remains). KEEP `caps_output_length`,
  `keeps_short_output_intact`, and `truncation_respects_char_boundaries`
  (`truncate_output` is unchanged).
- RUN `cargo test -p jcode-app-core tool::webfetch` — expect RED
  (compile failures against the new input shape and helpers, plus references
  to deleted items).

### Step 3S (source — file paths + line-by-line changes, all in
`crates/jcode-app-core/src/tool/webfetch.rs` unless noted)

1. Line 4 (`use futures::StreamExt;`): delete (no more byte streaming).
   Keep `Duration` (used for the 60s timeout).
2. Lines 9-18 (consts): delete `MAX_SIZE`, `MAX_URL_CHARS`,
   `DEFAULT_TIMEOUT`, `MAX_TIMEOUT`. Keep `MAX_OUTPUT_CHARS` and its doc
   comment. Timeout/endpoint constants now live in `tool/parallel.rs`.
3. Lines 33-39 (`struct WebFetchInput`): replace with `urls: Vec<String>`,
   `#[serde(default)] objective: Option<String>`, `#[serde(default)]
   search_queries: Option<Vec<String>>`, `#[serde(default)] full_content:
   Option<bool>`.
4. `description()` (line ~47): change the string to
   `"Fetch page content using Parallel."`.
5. Lines 52-72 (`parameters_schema`): `required` becomes `[urls]`; `urls` is
   a string array with `minItems: 1, maxItems: 20`; `objective` optional
   string; `search_queries` optional string array whose description quotes
   the API wording about focusing excerpts; `full_content` optional boolean
   ("when true, request full page content instead of excerpts"). Delete
   `url`/`format`/`timeout`. Keep `intent`.
6. Lines 75-178 (`execute`): replace the whole body. New body order:
   deserialize -> validate (1..=20 urls; every url starts with `http://` or
   `https://`, reusing the old error text) -> resolve key -> build body
   (`urls`, plus `objective`/`search_queries` when present and non-blank,
   plus `advanced_settings: {full_content: true}` only when the flag is true;
   `// TODO(later)` comment for `session_id`/`client_model` at the build
   site) -> `POST PARALLEL_EXTRACT_URL` with `x-api-key`, the JCode UA
   override, and 60s timeout -> 422 shape goes through `render_api_error` ->
   render each result and each error entry inline per the Exact output
   templates -> existing `truncate_output` + truncation note ->
   `ToolOutput::new`. No content-type sniffing, no size pre-check, no format
   branches.
7. Lines 182-196 (`truncate_output`): keep unchanged (with its three kept
   tests).
8. Lines 198-302 (`mod html_regex`, incl. `CHROME_TAGS`): delete the whole
   module.
9. Lines 304-345 (`html_to_text`): delete the whole function.
10. Lines 357-367 (`render_link`): delete the whole function.
11. Lines 369-452 (`html_to_markdown`): delete the whole function.
12. Lines 454-456 (`#[cfg(test)] #[path = "webfetch_corpus_tests.rs"] mod
    corpus_tests;`): delete both lines.
13. Delete file `crates/jcode-app-core/src/tool/webfetch_corpus_tests.rs`.
14. Delete file `scripts/webfetch_corpus.sh`.
15. Add `ParallelExtractResponse` / `ParallelExtractResult` /
    `ExtractErrorEntry` response structs (`Deserialize`, mirroring the
    nullable/required split in the API contract above) and a pure
    `render_extract_output(response) -> String` fn so the multi-section +
    inline-error rendering is unit-testable without networking.
- RUN `cargo test -p jcode-app-core tool::webfetch` — expect GREEN.

---

## Phase 4: Upstream callers (each: tests first, then the one-line swap)

### 4a. TUI summaries — `crates/jcode-tui/src/tui/ui_tools.rs`

- T step (descriptions only): a webfetch call carrying several urls
  summarizes as the first-three hostnames (each capped) with a "+ N more"
  suffix; a single-url call still summarizes as today's truncated full URL;
  a websearch call summarizes as its objective text; when the objective is
  missing the first search query is used instead. Malformed urls never panic
  the summarizer (graceful fallback to truncated raw text).
  RUN `cargo test -p jcode-tui ui_tests::tools` — expect RED (fields do not
  exist yet; the `webfetch-wide`/`websearch` rows in
  `test_common_tool_summaries_keep_full_text_when_row_budget_fits` fail).
- S step, lines 1125-1141 (the `"webfetch"` / `"websearch"` match arms):
  replace the `input.get("url")` arm with a call to a new
  `summarize_urls(urls, max_width)` helper, and the `input.get("query")` arm
  with `objective`-first/`search_queries[0]`-fallback logic. Place
  `summarize_urls` directly after `truncate_url_display` (ends line ~828) and
  implement it on top of the existing truncate helpers: single url ->
  `truncate_url_display(url, bounded(50))`; multiple -> map first three
  through host-extraction (text after `://` up to the next `/`, else the raw
  string), truncate each host to 16 chars, join with `", "`, append
  `" + {len-3} more"`. Keep the existing empty-input early return in
  `get_tool_summary_with_budget` (lines ~895-905) untouched.
- Rerun — expect GREEN.

### 4b. Storage compaction — `crates/jcode-tui/src/tui/app/state_ui_storage.rs`

- T step (descriptions only; existing tests start line 498): update the two
  web fixtures so the webfetch one carries a multi-url input plus format-less
  fields and asserts the compacted record keeps the urls while dropping
  everything else; the websearch one carries objective + search queries and
  asserts both survive compaction; the transcript summary derived from each
  compacted record still names the fetched host / search objective.
- RUN `cargo test -p jcode-tui state_ui_storage` — expect RED (whitelist
  keeps the old keys).
- S step, lines 103-110 (`"webfetch"` arm of
  `compact_tool_input_for_display`): keep the `urls` array cloned wholesale
  (truncate each entry to 200 chars via the existing `truncate_str`, mirroring
  today's per-string handling) instead of `url`. Lines 111-118
  (`"websearch" | ...` arm): this arm is SHARED with `codesearch`,
  `session_search`, `conversation_search` — split `"websearch"` out into its
  own arm keeping `objective` + `search_queries` (each truncated to 200
  chars), leaving the other three tools on `query` untouched.
- Rerun — expect GREEN.

### 4c. Inline marker — `crates/jcode-app-core/src/agent/inline_tail.rs`

- T step (descriptions only; existing marker tests live in `mod tests` from
  line 181, e.g. `marker_prefers_intent_over_raw_input` at 223): a websearch
  marker without intent shows the objective; a webfetch marker without intent
  shows the same multi-url summary as the TUI (first url / host list);
  existing intent-takes-precedence behavior is unchanged for both tools.
- RUN `cargo test -p jcode-app-core inline_tail` — expect RED.
- S step, lines 133-140 (the field-name `match`): replace the
  `"agentgrep" | "websearch" => "query"` combined arm so `websearch` maps to
  `"objective"` (agentgrep keeps `"query"`), and replace the
  `"webfetch" => "url"` arm with logic reading the `urls` array through the
  same summarize-first-url(s) rule (duplicate the tiny rule here rather than
  reaching into TUI code — app-core must not depend on the TUI crate).
  Truncation via `MAX_SUMMARY_CHARS` (line 25) below stays as-is.
- Rerun — expect GREEN.

### 4d. Harness + event fixtures (no unit tests to write; compile-guided)

- `src/bin/harness.rs`, lines 162-173: replace the webfetch case input with
  `{"urls": ["https://example.com", "https://example.org"],
  "objective": "Summarize the example domains"}` and the websearch case input
  with `{"objective": "rust async await",
  "search_queries": ["rust async await", "rust tokio tutorial"],
  "max_results": 5}`. (Labels stay as-is.) Verify with
  `cargo check --bin harness`.
- `crates/jcode-tui/src/tui/ui_tests/tools.rs`, `webfetch-wide` fixture
  (~line 1038): replace `{"url": ...}` with the new `{"urls": [...]}` shape
  (single url, so the expected summary string is unchanged text).
- `crates/jcode-tui/src/tui/app/tests/remote_events_reload_02/part_02.rs`,
  streaming deltas + assertions (~lines 378-442): replace
  `{"url":"https://example.com/a",...}` deltas with
  `{"urls":["https://example.com/a"],...}` shapes and the `input.get("url")`
  assertion with an `input.get("urls")` array assertion. The test's PURPOSE
  (sibling input surviving `ToolDone`) is unchanged — only shapes move.
- RUN the two TUI test targets:
  `cargo test -p jcode-tui test_common_tool_summaries_keep_full_text_when_row_budget_fits`
  and
  `cargo test -p jcode-tui test_tool_done_preserves_sibling_streaming_tool_inputs_and_intents`
  — expect GREEN (these fixture edits land
  together with their source changes, so no red step applies; the red
  coverage for them came from 4a/4b).

---

## Phase 5: Old-engine + old-config + stale-docs deletion

### Step 5T (tests first — descriptions only)

- Update to the liveness test from Phase 1T: it now asserts the REMOVED
  `websearch.*` keys resolve as non-live/unknown and the `[parallel]` key
  resolves as live. No new test files.
- RUN `cargo test -p jcode-base config::change_report` — expect RED (old
  keys still present / new expectations unmet).

### Step 5S (source deletions, line-by-line)

1. `crates/jcode-config-types/src/lib.rs`, lines 1148-1216: delete
   `WebSearchEngine` (incl. `as_str`/`parse`), `WebSearchConfig`, and its
   `Default` impl. Keep the new `ParallelConfig` from Phase 1.
2. `crates/jcode-base/src/config.rs`, lines 486-488: delete the `websearch:
   WebSearchConfig` field + doc comment.
3. `crates/jcode-base/src/config/default_file.rs`: delete the leftover
   `[websearch]` block (the `[parallel]` block from Phase 1 stays).
4. `crates/jcode-base/src/config/env_overrides.rs`, lines 501-535: delete the
   `JCODE_WEBSEARCH_ENGINE`, `JCODE_WEBSEARCH_FALLBACK_ENGINES`,
   `JCODE_BING_API_KEY`, `JCODE_BING_API_KEY_ENV`, `JCODE_BING_MARKET`,
   `JCODE_SEARXNG_URL` blocks. Keep the Phase 1 `JCODE_PARALLEL_API_KEY`
   block.
5. `docs/DISCOVERY_CONVERSION_ANALYSIS.md`: update the web-data conclusion
   that attributes discovery traffic to blocked `websearch` (conclusion 5 and
   the 4-queries note) with a dated one-liner noting the Parallel migration;
   do not rewrite the historical analysis.
6. Confirm zero stragglers: `grep -rn "num_results\|bing_market\|searxng\|
   duckduckgo\|WebSearchEngine\|WebFetchInput\|webfetch-corpus\|html_to_markdown" crates/ src/ scripts/`
   must return nothing (excluding this plan doc).
- RUN the config tests + `cargo check --workspace` — expect GREEN.

## Phase 6: Verification (no new tests)

1. `cargo test -p jcode-app-core tool::` (both tools' new fixtures).
2. `cargo test -p jcode-tui` (summaries, compaction, remote-events).
3. Full suite ONCE at the very end.
4. Manual live smoke only via `src/bin/harness.rs --include-network` with
   `PARALLEL_API_KEY` set (keyed + billable: never in automated tests).

## Files to Create/Modify (final list)

1. `crates/jcode-app-core/src/tool/parallel.rs` — NEW (key, URLs, timeout,
   error rendering)
2. `crates/jcode-app-core/src/tool/mod.rs` — one-line `mod parallel;`
3. `crates/jcode-config-types/src/lib.rs` — add `ParallelConfig`, delete old
   search types
4. `crates/jcode-base/src/config.rs` — swap `websearch` field for `parallel`
5. `crates/jcode-base/src/config/default_file.rs` — `[parallel]` section
6. `crates/jcode-base/src/config/env_overrides.rs` — `JCODE_PARALLEL_API_KEY`
7. `crates/jcode-app-core/src/tool/websearch.rs` — rewrite (Phase 2)
8. `crates/jcode-app-core/src/tool/webfetch.rs` — rewrite (Phase 3, keep
   `truncate_output`)
9. `crates/jcode-tui/src/tui/ui_tools.rs` — summaries + `summarize_urls`
10. `crates/jcode-tui/src/tui/app/state_ui_storage.rs` — whitelist + tests
11. `crates/jcode-app-core/src/agent/inline_tail.rs` — marker map
12. `src/bin/harness.rs` — example inputs
13. `crates/jcode-tui/src/tui/ui_tests/tools.rs` — fixture
14. `crates/jcode-tui/src/tui/app/tests/remote_events_reload_02/part_02.rs`
15. `crates/jcode-base/src/config/change_report_tests.rs` — key list
16. `docs/DISCOVERY_CONVERSION_ANALYSIS.md` — dated stale-conclusion note
17. DELETE: `.../tool/webfetch_corpus_tests.rs`,
    `.../tool/testdata/ddg_anomaly.html`,
    `.../tool/testdata/ddg_results.html`, `scripts/webfetch_corpus.sh`

## Order of Implementation

Phase 1 (config + helper) -> Phase 2 (websearch) -> Phase 3 (webfetch) ->
Phase 4 (callers) -> Phase 5 (deletions + stale docs) -> Phase 6
(verification).
Each phase: T tests fail -> S source lands -> tests green. Never invert.
