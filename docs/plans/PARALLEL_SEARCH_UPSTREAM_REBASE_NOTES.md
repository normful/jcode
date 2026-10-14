# Parallel websearch/webfetch: rebase onto upstream v0.86+ notes

Records what happened when the Parallel migration branch was rebased from its
original base (`e589cbe5a`, v0.86.0) onto `upstream/master` `1f612a7d3`
(2026-10-14, 782 commits later), so the resolution decisions are reviewable
instead of buried in conflict hunks.

Branch: `parallel-search-on-upstream-master`.

## Commits

The branch carried four commits; three survived.

| original | result |
|---|---|
| `4ec5753b5` docs(plans): implementation plan | kept |
| `88c43bb35` docs(plans): revise plan after review | kept |
| `f4630c1cc` feat!: migrate to Parallel | kept, conflicts resolved (see below) |
| `f56835800` fix(bench): repair tui_bench | **dropped** — upstream already fixed it |

`f56835800` was dropping a stale `diff_line_wrap` impl and adding
`focus_revision: 0`. Upstream's `src/bin/tui_bench.rs` already contains both,
so the patch was empty and git dropped it with `--empty=drop`.

## What upstream changed in the same area

Upstream did not stand still on this code:

- `c427cc191` (2026-09-30) provider-native server-side search — `engine =
  "native"`, `prefer_native`, Anthropic `web_search` and OpenAI Responses
  `web_search`, with the local tool swapped out for the hosted one.
- `3942ceee3` (2026-09-30) never swap an SDK custom `websearch` tool, only the
  built-in one. Detection is by exact description match against
  `LOCAL_WEBSEARCH_DESCRIPTION`.
- `3f0447c51` (2026-10-13) configurable custom HTTP JSON search providers.
- `4687f291b` (2026-10-14) `websearch` unit tests extracted to
  `websearch_tests.rs` via `#[path]`.
- `open.rs` rewritten: `open_target`/`reveal_target` became
  `perform_open`/`perform_reveal` returning `OpenOutcome`.

## Conflict resolutions

The migration commit conflicted in 13 files. Two classes:

**Cosmetic-only (13 of the hunks).** The migration commit carried clippy 1.96
cleanups (`needless_return`, collapsible `if let` chains,
`manual_is_multiple_of`, a `needless_lifetimes` elision, a type alias for a
complex static). Upstream had already fixed or rewritten the same lines, so
each conflicted hunk was resolved by taking upstream's version and dropping
ours: `tool/open.rs`, `server/client_lifecycle.rs`, `server/comm_session.rs`,
`core/stdin_detect.rs`, `tui-mermaid/src/lib.rs`, `cli/commands/menubar.rs`,
`cli/login/scriptable.rs`, and one comment in `setup-hints/src/lib.rs`. Our
non-conflicting hunks in those files were kept.

**Substantive (websearch and its config).** The migration deleted every local
search engine. Upstream has meanwhile grown local engines *and* provider-native
search, which are different things. Resolved as:

- **Kept provider-native search whole.** It is orthogonal to which HTTP API
  serves a local query, it lives in the provider runtimes, and it is the
  feature upstream invested in. `WebSearchConfig` survives with only its
  native knobs: `prefer_native`, `native_max_uses`, `native_allowed_domains`,
  `native_blocked_domains`, `native_anthropic_tool_version`.
- **Deleted the local engines**: `WebSearchEngine`, `CustomWebSearchConfig`,
  `CustomWebSearchMethod`, and the `engine`, `fallback_engines`, `bing_*`,
  `searxng_*` config fields, with their env overrides
  (`JCODE_WEBSEARCH_ENGINE`, `JCODE_WEBSEARCH_FALLBACK_ENGINES`,
  `JCODE_BING_*`, `JCODE_SEARXNG_URL`) and `CONFIG_ENV_KEYS` entries.
  `native_enabled()` is now just `prefer_native`.
- `[websearch]` survives in the config template with the native knobs plus a
  pointer at `PARALLEL_API_KEY`.

### The one upstream feature this discards

**The custom HTTP JSON search provider (`3f0447c51`) is dropped.** It is a
local, non-scraping backend, but it is still a second local backend: it needs a
`custom_request`/`parse_custom_results` code path plus a `query`-shaped input
that the Parallel schema (`objective` + `search_queries`) no longer has, so
keeping it means inventing a mapping from `objective`/`search_queries` onto a
single query string. That is a design decision, not a rebase mechanic, so it
was left out. `docs/custom-web-search.md` was removed with it. Re-porting is
one focused commit if wanted.

### Config compatibility

Removing keys from `[websearch]` does not break existing config files: `Config`
does not set `serde(deny_unknown_fields)`, so an old file full of
`engine = "duckduckgo"` and `fallback_engines = ["bing"]` still loads and those
keys are ignored. This is pinned by
`config::legacy_websearch_engine_keys_are_ignored_not_rejected`.

## Behaviour change worth a decision

`prefer_native` defaults to **true**. On an Anthropic first-party key or the
OpenAI Responses API, provider-native search therefore replaces the built-in
`websearch` tool, and the Parallel path is never used on those routes unless
`prefer_native = false` is set. That default predates this branch and was kept
as-is; it does mean the Parallel backend serves Copilot, OpenRouter, Bedrock
and other non-first-party routes first. Whether Parallel should instead take
precedence is a product call, not a rebase call.

## Verification performed

- `cargo check --workspace --all-targets` — clean, no warnings from the touched
  crates.
- `cargo test --workspace --lib` filtered over `websearch`, `webfetch`,
  `parallel`, `native_web_search`, `web_tool_summaries`,
  `compaction_keeps_web`, `inline_tail`, `marker_shows` — all pass.
- `cargo test -p jcode-base --lib -- config` — 168 pass, including the new
  template and legacy-key tests.
- `cargo test -p jcode-provider-gemini-runtime --test
  every_provider_sends_clean_schemas` and `cargo test -p jcode-schema-dialect`
  — pass, so the new array schemas survive the schema dialects.

### Full-suite failures do not come from this branch

A whole-workspace `cargo test --workspace --lib --no-fail-fast` reports
failures in unrelated crates (`auth::transfer`, `worktrees`, TUI goldens,
`compile_remote`, session-context cases). None of them touch the changed
surface, and they were checked against a pristine `upstream/master` worktree:

- `jcode-base`: the same 11 failures on upstream, by name.
- `jcode-terminal-launch`: the same 3. The root cause is ambient environment,
  not code: the test sets `GHOSTTY_RESOURCES_DIR` but clears neither
  `HERDR_ENV` nor `HERDR_PANE_ID`, and detection now prefers herdr, so it fails
  when jcode runs inside a herdr pane. The other two are mutex-poisoning
  cascades from that first failure.
- `jcode-setup-hints`: the same 1. `SetupHintsState::default()` sets
  `startup_spawn_hint_dismissed: true`, while the test builds its state from
  `..Default::default()` and then expects a hint; the function it exercises is
  untouched here.
- `jcode-app-core`, `jcode`, `jcode-harness-api-server`, `jcode-provider-bedrock`,
  `jcode-sdk`, `jcode-tui`: the same failure names reproduce on upstream. The
  two app-core failures on this branch that upstream does not show are
  `test_openai_supports_codex_models` (cross-crate interference — it passes in
  isolation on both trees) and `mcp_startup_tests::headless_mcp_cold_wait...`
  (an ordering assertion, flaky).
- The two cap failures name only `bash` (~53 tokens) and `macos_computer_use`
  (~159 tokens, 93 of them in one parameter). Both descriptions are
  byte-identical upstream and neither file is touched here, so this is a
  pre-existing, macOS-only failure. The migrated tools are inside the caps:
  `websearch` 18 and `webfetch` 13, against a cap of 20.

### Environment caveats (not caused by this branch)

- This machine's `cc` is GCC 15.3.0, not Apple clang, so the `aws-lc-sys`
  (BoringSSL) C build fails. Upstream's lock also contains `aws-lc-sys`, via
  `aws-config` → the Bedrock provider. Builds here need
  `CC=/usr/bin/cc CXX=/usr/bin/c++ SDKROOT=$(xcrun --show-sdk-path)`.
- `tool::tests::tool_descriptions_stay_under_token_cap` fails on macOS for
  `bash` (~53 tokens) and `macos_computer_use` (~159 tokens). Both descriptions
  are byte-identical to upstream and neither file is touched here, so this is
  pre-existing and platform-specific. The two migrated tools are within the cap:
  `websearch` 18, `webfetch` 13.
