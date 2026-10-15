# Plan: overridable Parallel API origin for the local `websearch` / `webfetch` tools

> Status: Design complete, all review decisions recorded 2026-10-15.
> Implemented 2026-10-15 (Phases 1-3). Uncommitted — see Implementation log.
>
> How to read this plan: each phase is red-green. The **T step** lists test cases
> to write FIRST (behavior descriptions only) and the check that must FAIL before
> proceeding. The **S step** lists the source changes (file paths + line-level
> edits) that turn them green. Do not write source before its tests fail.
>
> Written against `4d9da32d0` (branch `rebase`), which carries `793326689`
> ("feat!: migrate websearch/webfetch to the Parallel Search + Extract APIs")
> unchanged.

## Problem

Both local web tools post to hardcoded URLs:

```rust
// crates/jcode-app-core/src/tool/parallel.rs:8,10
pub const PARALLEL_SEARCH_URL:  &str = "https://api.parallel.ai/v1/search";
pub const PARALLEL_EXTRACT_URL: &str = "https://api.parallel.ai/v1/extract";
```

used at exactly two call sites, `websearch.rs:104` and `webfetch.rs:123`, both
`execute()` → `execute_with_endpoint(input, ctx, <const>)`. A user who must route
that traffic through a corporate egress proxy, a caching gateway, or a local
recorder cannot: the only consumer of the origin is the constant, and there is no
knob anywhere in `[websearch]` (the migration deliberately left that section with
provider-native search options only).

The test seam already exists — `execute_with_endpoint` takes the URL as a
parameter and the tests pass mock-server URLs (`websearch.rs:432`,
`webfetch.rs:518`) — so the resolver only has to feed production `execute()`.

## Existing conventions this plan follows

| Precedent | Where | What we copy |
| --- | --- | --- |
| Multi-name env override, invalid values warned-and-skipped, env-only | `crates/jcode-base/src/provider/openai.rs:29-52` (`resolve_api_base`) | The `for var in OVERRIDE_VARS { … }` shape, the `logging::warn` on a bad value, the "blank means unset" rule |
| Proxy override for a built-in endpoint, validated, env-only, no config field, no login wiring | `docs/plans/BUILTIN_PROFILE_BASE_URL_OVERRIDE_PLAN.md` (implemented; documented at `docs/AUTH_CREDENTIAL_SOURCES.md:99`) | Staying env-only, reusing the repo's validator instead of writing a new one |
| `normalize_api_base` | `crates/jcode-provider-metadata/src/lib.rs:248-268`, reachable in app-core as `crate::provider_catalog::normalize_api_base` (re-export at `provider_catalog.rs:6`, `pub use jcode_base::*` at `crates/jcode-app-core/src/lib.rs:24`) | Accepts `https://` plus `http://` only for loopback / private / link-local / `.local` / `.lan`; strips a trailing `/`. Public plain-http is rejected, which is what keeps the API key off cleartext hosts. |
| `PARALLEL_API_KEY` is unprefixed and vendor-named | `parallel.rs:20` | Accepting the vendor's own base-URL name alongside `JCODE_*` is consistent, not novel |

Vendor-side grounding for the value's shape and names:

- Parallel's Python SDK reads **`PARALLEL_BASE_URL`** for the client root and
  appends `/v1/...` itself; the SDK's own example is
  `http://my.test.server.example.com:8083`.
- Parallel's CLI reads **`PARALLEL_API_URL`** for the data API (search, extract,
  research, …), default `https://api.parallel.ai` — again an origin.
- So an override is an **origin (optionally with a path prefix)**, never a
  versioned base. `https://api.parallel.ai/v1` is what a *client* is given, but it
  is not what an origin override may contain here (see the `/v1` footgun below).

## Design

One origin override for both endpoints, resolved per request, default unchanged.

```rust
// crates/jcode-app-core/src/tool/parallel.rs

/// Default Parallel API origin. The `/v1/<resource>` paths are appended by
/// `parallel_search_url` / `parallel_extract_url`, so an override names an
/// origin (optionally with a path prefix), never a versioned base.
const DEFAULT_PARALLEL_BASE_URL: &str = "https://api.parallel.ai";

/// Origin overrides, in precedence order: this repo's convention first, then the
/// vendor's own knob (the Python SDK reads `PARALLEL_BASE_URL`).
const PARALLEL_BASE_URL_VARS: [&str; 2] = ["JCODE_PARALLEL_API_BASE", "PARALLEL_BASE_URL"];

/// Resolve the Parallel API origin. First non-blank, valid override wins; blank
/// and invalid values are skipped (invalid also warns). Read per call like
/// `provider::openai::resolve_api_base`, so tests need no cache invalidation.
fn parallel_base_url() -> String {
    for var in PARALLEL_BASE_URL_VARS {
        let Ok(raw) = std::env::var(var) else { continue };
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

pub fn parallel_search_url() -> String {
    format!("{}/v1/search", parallel_base_url())
}

pub fn parallel_extract_url() -> String {
    format!("{}/v1/extract", parallel_base_url())
}
```

`PARALLEL_SEARCH_URL` / `PARALLEL_EXTRACT_URL` are deleted: an audit of the whole
tree shows their only consumers are the two `execute()` call sites (see the audit
table), and Phase 1 pins the default origin as a literal so there is one place the
default lives.

Call sites become:

```rust
// websearch.rs:104
self.execute_with_endpoint(input, ctx, &super::parallel::parallel_search_url()).await

// webfetch.rs:123
self.execute_with_endpoint(input, ctx, &super::parallel::parallel_extract_url()).await
```

The temporary `String` lives to the end of the statement, which includes the
`await`, so the `&str` parameter is fine.

### Assumptions this design makes

1. **The proxy speaks Parallel's wire protocol** — same `/v1/<resource>` paths,
   same `x-api-key` header, same search/extract JSON shapes. No request
   rewriting, no path remapping, no alternate auth.
2. **The proxy receives `PARALLEL_API_KEY` verbatim.** The override is opt-in and
   user-owned; `normalize_api_base` is what prevents the key from going to a
   public plain-http host.
3. **A broken proxy fails the tool call.** There is no silent fallback to
   `api.parallel.ai`; falling back would mask a misconfiguration while still
   spending the user's credits somewhere they did not intend.
4. **Env is read at request time, but the daemon's env is fixed at spawn.** For
   daemon-served sessions, changing the override needs the same restart as
   changing the key. A per-invocation `jcode run --socket …` picks it up
   immediately.

### /v1 footgun

Origin semantics plus a `/v1`-suffixed value gives `https://host/v1/v1/search`.
That is the one plausible typo, which is why it warns, and why the warning is a
warning and not a rewrite: stripping a user-supplied `/v1` would be guessing, and
a proxy legitimately mounted at `/v1/...` is not distinguishable from the typo.

## Audit: every path that reaches a Parallel endpoint

| Site | Honours the override | Note |
| --- | --- | --- |
| `parallel.rs:8,10` (constants) | Replaced by the resolver | Deleted; no other consumers |
| `websearch.rs:104` | Yes | Production `execute()` |
| `webfetch.rs:123` | Yes | Production `execute()` |
| `websearch.rs:20`, `webfetch.rs:24` (doc comments naming the constants) | Doc only | Reworded to name the resolver |
| `websearch.rs:419`, `webfetch.rs:505` (in-file mock helpers) | No, by design | They inject an explicit URL into `execute_with_endpoint`; that seam must keep working |
| `src/bin/harness.rs:163-176` (`--include-network`) | Yes, inherited | No change needed |
| `crates/jcode-base/src/config/default_file.rs:322-333` | Doc only | Template comment |
| Provider-native hosted search (Anthropic / OpenAI) | Not applicable | It never touches Parallel |

## Non-goals

- A config-file field (`[websearch] parallel_base_url`). The key is env-only and
  the provider-base precedent is env-only; a proxy origin is per-machine
  configuration.
- A credential or header knob for proxies that mint their own tokens
  (Review decision 3).
- Per-endpoint full-URL overrides (`…_SEARCH_URL` / `…_EXTRACT_URL`). The origin
  form covers path prefixes; widening later is additive.
- Retry, failover, or automatic fallback to the default origin.
- `CONFIG_ENV_KEYS` registration. The base URL is not config-cache input; adding
  it would force a full config reload whenever an unrelated web-tool var changes.
- TUI or `jcode login` prompts for the value.
- Any change to `PARALLEL_REQUEST_TIMEOUT_SECS`, `PARALLEL_MAX_CHARS_TOTAL`, the
  request bodies, or the response rendering.

## Phases

### Phase 1 — Resolver

**T step.** Add to `mod tests` in `crates/jcode-app-core/src/tool/parallel.rs`
(reuse the `restore_key` pattern at `:63`; every case holds
`crate::storage::lock_test_env()`, because env is process-global), run
`cargo test -p jcode-app-core --lib tool::parallel`, and confirm each new case
fails before the S step:

1. `web_urls_default_to_the_parallel_origin` — with both vars unset,
   `parallel_search_url() == "https://api.parallel.ai/v1/search"` and
   `parallel_extract_url() == "https://api.parallel.ai/v1/extract"`. This is the
   unchanged-by-default pin.
2. `override_replaces_the_origin_and_keeps_the_version_path` —
   `JCODE_PARALLEL_API_BASE=https://proxy.example.com` yields
   `https://proxy.example.com/v1/search` and `…/v1/extract`.
3. `override_keeps_a_path_prefix_and_strips_a_trailing_slash` —
   `https://proxy.example.com/parallel/` yields
   `https://proxy.example.com/parallel/v1/search`.
4. `vendor_alias_is_honoured_and_the_jcode_name_wins` — `PARALLEL_BASE_URL` alone
   selects the vendor host; setting both selects the `JCODE_` one.
5. `blank_override_is_skipped_not_fatal` — `JCODE_PARALLEL_API_BASE=""` (and
   `"   "`) falls through to `PARALLEL_BASE_URL` when that is set, and to the
   default when it is not.
6. `invalid_override_falls_back_to_the_default` — `"notaurl"`, `"ftp://x"`, and
   `"http://public.example.com"` each leave the default origin in place. The last
   case is the key-safety property: no public plain-http host.
7. `loopback_plain_http_override_is_allowed` — `http://127.0.0.1:8080` is
   accepted, which is what a local recorder or test proxy needs.

**S step.** In `crates/jcode-app-core/src/tool/parallel.rs`: replace the two URL
constants (`:8`, `:10`) with `DEFAULT_PARALLEL_BASE_URL` and
`PARALLEL_BASE_URL_VARS`; add `parallel_base_url`, `parallel_search_url` and
`parallel_extract_url` above `parallel_api_key` (`:20`); extend the module doc
(`:1-5`) to state that the origin is overridable through the two env vars, that
the same value is what makes a proxy receive the API key, and that nothing is
sent to the override host unless it is set.

### Phase 2 — Tool call sites and end-to-end coverage

**T step.** In `crates/jcode-app-core/src/tool/websearch.rs` and
`…/webfetch.rs`, after extending the mock helpers (below), and confirming each
case fails first:

1. `search_execute_honours_the_base_override` — with the key set and
   `JCODE_PARALLEL_API_BASE` set to the mock origin, `WebSearchTool::execute()`
   (the production entry point, not `execute_with_endpoint`) returns the rendered
   results, and the captured request line is `POST /v1/search`.
2. `fetch_execute_honours_the_base_override` — same for `WebFetchTool::execute()`
   and `POST /v1/extract`.
3. `search_and_fetch_ignore_an_invalid_base_override` — with an invalid override
   set, the resolver-derived URL is the default (asserted on
   `parallel_search_url()`), so a typo cannot silently redirect a live request.

Mock helper change required by cases 1 and 2: `mock_search_server`
(`websearch.rs:376-419`) and `mock_extract_server` (`webfetch.rs:462-505`)
currently capture only the request body, so the appended path cannot be observed.
Return a third element — `Arc<Mutex<String>>` holding the raw request line (first
line of the HTTP request) — keeping the existing body capture untouched. The
helpers return a full endpoint URL (`websearch.rs:419`, `webfetch.rs:505`), so a
test derives the origin with `url.trim_end_matches("/v1/search")` (resp.
`"/v1/extract"`); note that in a comment, since it is the one non-obvious step.

**S step.** `websearch.rs:103-106` and `webfetch.rs:122-125`: pass
`&super::parallel::parallel_search_url()` / `parallel_extract_url()`; reword the
`execute_with_endpoint` doc comments that name the old constants
(`websearch.rs:19-21`, `webfetch.rs:23-25`); add a `restore_parallel_base`
helper beside each `restore_parallel_key` (`websearch.rs:276`, `webfetch.rs:330`)
that restores both `JCODE_PARALLEL_API_BASE` and `PARALLEL_BASE_URL`.

Existing tests must keep passing unchanged: they call `execute_with_endpoint` with
an explicit mock URL, which is the regression guard for the seam.

### Phase 3 — Documentation

**T step.** Extend `default_config_template_drops_local_websearch_engines`
(`crates/jcode-base/src/config/default_file.rs:835` asserts the template mentions
`PARALLEL_API_KEY`) to also assert it mentions `JCODE_PARALLEL_API_BASE`. Run
`cargo test -p jcode-base --lib -- config` and confirm it fails first.

**S step.** In the `[websearch]` template comment (`default_file.rs:322-333`), add
two or three lines: local search and fetch go to `https://api.parallel.ai` by
default, `JCODE_PARALLEL_API_BASE` (or the vendor's `PARALLEL_BASE_URL`) points
them at a proxy origin, `/v1/search` and `/v1/extract` are appended, and the API
key is sent to whatever host that names. No login or TUI changes.

Optionally, a short subsection beside the existing provider-proxy section at
`docs/AUTH_CREDENTIAL_SOURCES.md:99` — that file documents proxy redirects for
provider endpoints, and this is the same question asked about the web tools. Not
required for correctness; the template is where `PARALLEL_API_KEY` is already
documented.

## Validation matrix

- `cargo test -p jcode-app-core --lib tool::parallel`
- `cargo test -p jcode-app-core --lib tool::websearch`
- `cargo test -p jcode-app-core --lib tool::webfetch`
- `cargo test -p jcode-base --lib -- config` (template assertions)
- `cargo check --workspace --all-targets`
- Unchanged-by-default: with neither var set, `parallel_search_url()` equals the
  literal that used to live in `PARALLEL_SEARCH_URL` (Phase 1 case 1).
- Manual, proxy route: run a local recorder on `127.0.0.1:8080`, then
  `JCODE_PARALLEL_API_BASE=http://127.0.0.1:8080 PARALLEL_API_KEY=… ./target/selfdev/jcode run --no-update --socket /tmp/jcode-proxy.sock '<a prompt that searches>'`
  and confirm `POST /v1/search` and `POST /v1/extract` arrive at the recorder with
  the `x-api-key` header. The separate socket matters: per `AGENTS.md`, a daemon
  reads its environment at spawn, so an override set in the caller's shell does
  not reach an already-running shared daemon.

## Risks

- **The key is sent to the override host.** Opt-in, documented, and plain-http
  outside private networks is rejected — but a typo that names a valid https host
  still ships the key there. The invalid-value warning says so explicitly.
- **`PARALLEL_BASE_URL` is a generic vendor name that a machine may already set**
  for another Parallel client (the Python SDK honours it too). Accepting it means
  jcode silently follows that value. That is the point — one variable configures
  every Parallel client on the box — but it means "unexpected base" is possible
  without anyone setting a `JCODE_` variable. Recorded as Review decision 2.
- **`/v1/v1` typo.** Warned, not rewritten (see above).
- **Env-global tests.** Every new case mutates process env; missing
  `crate::storage::lock_test_env()` produces cross-test flakiness rather than a
  clean failure. Both T steps require it, mirroring the existing
  `parallel_api_key` tests.
- **A proxy that does not speak the wire protocol** fails with the proxy's own
  error text, which surfaces through the existing `render_api_error` path.

## Review decisions (2026-10-15)

1. **Override value is an origin**, optionally with a path prefix; jcode appends
   `/v1/search` and `/v1/extract`. Matches `PARALLEL_BASE_URL` / `PARALLEL_API_URL`
   semantics rather than this repo's version-inclusive `JCODE_*_API_BASE` style.
2. **Env only, two names**: `JCODE_PARALLEL_API_BASE` first,
   `PARALLEL_BASE_URL` second. No config-file field.
3. **Transparent pass-through assumed**: the proxy accepts Parallel's
   `x-api-key`; no credential or header knob.

## Open Questions

- **Should a successful non-default resolution be logged?** Per-request `info`
  would be audit-friendly but chatty; `resolve_api_base` logs nothing on success.
  `Default: silent on success, warn only on invalid values.`
- **Should the vendor alias be honoured at all?** It buys shared configuration
  with Parallel's own SDK and CLI, at the cost of inheritance nobody typed into
  this repo's config. `Default: honour it, and say so in the template comment.`
- **Is the two-URL escape hatch needed?** Some proxies rewrite the version path
  rather than prefixing it. `Default: not now; origin covers prefixes, and adding
  per-endpoint URL variables later is additive.`
- **Manual validation with a real proxy was not run.** `Default: leave it for
  implementation; the automated cases use loopback mock servers, which exercise
  the same resolve → `execute()` path.`
- **Implementation status.** This plan is written on request, not yet executed.
  `Default: implement Phases 1-3 as written and leave them uncommitted so a review
  that wants different decisions costs one git checkout.`

## Implementation log (2026-10-15, agent)

Phases 1-3 implemented as written and left uncommitted, so a review that wants
other decisions costs one `git checkout`. Deviations and their evidence:

- **Phase 2 mock shape.** The mock helpers now return
  `(url, body, request_line, header_lines)` through three `type` aliases
  (`CapturedBody`, `CapturedRequestLine`, `CapturedHeaderLines`) rather than the
  T step's inline 3-tuple: the literal tuple trips `clippy::type_complexity`, CI
  runs `cargo clippy --all-targets -- -D warnings`, and the aliases are clippy's
  own suggested fix. The fourth element is a review addition — see below.
- **Red-green evidence.** Phase 1 red = compile error for
  `parallel_search_url` / `parallel_extract_url` / `PARALLEL_BASE_URL_VARS`;
  Phase 3 red = the new `JCODE_PARALLEL_API_BASE` template assertion failing.
  Phase 2 red was observed by temporarily pinning both call sites back to the
  default literal, which made the two new `execute()` tests reach the real
  `api.parallel.ai` and fail with `401 Invalid API key` — i.e. they do measure
  the override path, not just the mock URL.
- **The manual recorder check was run, deterministically.** Rather than a live
  `jcode run` session, the harness drives the same production path:
  `cargo build --bin jcode-harness`, then
  `JCODE_PARALLEL_API_BASE=http://127.0.0.1:8080 PARALLEL_API_KEY=recorder-key`
  `./target/debug/jcode-harness --include-network` against
  `target/tmp/parallel_recorder.py`. The recorder logged
  `POST /v1/extract HTTP/1.1` and `POST /v1/search HTTP/1.1`, each with
  `x-api-key: recorder-key`. Negative control: with the override unset the
  recorder logged nothing and both tools reached `api.parallel.ai` (401 with the
  dummy key), confirming the default origin is unchanged in the real binary.
- **Not automated before the review.** Only the recorder run proved `x-api-key`
  reaches the override host; no unit test asserted the header.
- **Review follow-ups (2026-10-15), all four applied.**
  1. The mocks now also capture the header lines, and both new `execute()` tests
     assert `header_value(&headers, "x-api-key") == Some("test-key")`, so the one
     property the recorder run uniquely proved is now a unit-test assertion. The
     assertion cannot pass vacuously: a broken capture yields `None`.
  2. `crates/jcode-tui/src/tui/ui_messages.rs` is reverted to upstream
     `1c7426eb1`'s form, which makes the whole file byte-identical to upstream
     v0.98.1 again (`diff` against `git show 1c7426eb1:…` is empty). Its
     covering test `render_todo_plan_update_card_shows_only_changed_intent_fields`
     passes.
  3. `docs/AUTH_CREDENTIAL_SOURCES.md` gained a "Redirecting the local web tools'
     Parallel origin" section beside the provider base-URL override section.
  4. The code files and this plan are committed together.
- **`ui_messages.rs` carry-over** (handoff Learning 1). `git blame` does
  attribute the behaviour-neutral rewrite of `render_todo_plan_update`'s guard to
  `793326689`, and upstream `1c7426eb1` has the other form. Reverted per the
  review — see below.
- **Validation run (final).** `cargo test -p jcode-app-core --lib -- tool::parallel
  tool::websearch tool::webfetch` → 37 passed;
  `cargo test -p jcode-base --lib -- config` → 169 passed;
  `cargo test -p jcode-tui --lib -- render_todo_plan_update` → 1 passed;
  `cargo check --workspace --all-targets` clean; `cargo fmt --all -- --check`
  clean; clippy adds no lint beyond 2 further `await_holding_lock` warnings,
  which the pre-existing env-mutating async tests in those same modules already
  produce (5 before, 7 after), and no `type_complexity`. Full
  `cargo test -p jcode-app-core --lib` shows 5 deterministic pre-existing failures
  plus 2 order-flaky ones, all reproduced on the unmodified tree.
- **Machine caveat.** `aws-lc-sys` does not build with the Nix `cc` (GCC 15.3.0).
  Every command above ran with
  `CC=/usr/bin/clang CXX=/usr/bin/clang++ SDKROOT=$(xcrun --show-sdk-path)`.
