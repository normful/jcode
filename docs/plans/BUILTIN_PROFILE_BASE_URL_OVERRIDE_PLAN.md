# Plan: Opt-in base-URL override for selected built-in OpenAI-compatible profiles

> Status: Design complete, all review decisions recorded 2026-10-15.
> Not implemented.
>
> How to read this plan: each phase is red-green. The **T step** lists test cases
> to write FIRST (behavior descriptions only) and the check that must FAIL before
> proceeding. The **S step** lists the source changes (file paths + line-level
> edits) that turn them green. Do not write source before its tests fail.

## Problem

A user who must route provider traffic through a corporate or self-hosted proxy —
or who wants to record and inspect the exact wire requests — cannot point the
built-in OpenCode Go profile anywhere but `https://opencode.ai/zen/go/v1`.

Trace of why the existing knobs miss it:

1. `OpenAiCompatibleProfile::api_base` is a compile-time constant
   (`crates/jcode-provider-metadata/src/catalog.rs:17-26`).
2. The generic override `JCODE_OPENAI_COMPAT_API_BASE` is applied only to the
   profile whose id is `openai-compat`: `resolve_openai_compatible_profile_endpoint`
   returns early for every other profile
   (`crates/jcode-base/src/provider_catalog.rs:81-83`), after applying only the
   MiniMax key heuristic (`:79`, `:254-277`).
3. The TUI base-URL editor and the interactive `login` prompt write that same
   compat-only variable
   (`crates/jcode-tui/src/tui/app/auth_account_commands.rs:826-848`,
   `src/cli/login.rs:843-878`), and the prompt is gated on
   `is_custom_profile` (`src/cli/login.rs:834`).
4. The runtime-level variable `JCODE_OPENROUTER_API_BASE` *is* honoured
   (`crates/jcode-provider-openrouter-runtime/src/lib.rs:146-153`), but every path
   that selects a built-in profile rewrites it from the constant — unconditionally
   in the login and explicit-`--provider` paths, which call
   `force_apply_openai_compatible_profile_env`
   (`crates/jcode-base/src/provider_catalog.rs:758-760`, from
   `src/cli/provider_init.rs:1326` and `crates/jcode-base/src/auth/lifecycle.rs:1278`).

The only workaround today is to preset `JCODE_OPENROUTER_API_BASE`,
`JCODE_OPENROUTER_API_KEY_NAME`, `JCODE_OPENROUTER_ENV_FILE`,
`JCODE_OPENROUTER_CACHE_NAMESPACE=opencode-go` and
`JCODE_PROVIDER_PROFILE_ACTIVE=1`, and to hope no force-apply path runs. It is
undocumented, and `jcode login opencode-go` breaks it.

## Why the Responses API constrains the design

OpenCode Go serves a small model subset through the OpenAI **Responses** API
(`POST <base>/responses`) while the rest of its catalog uses
`/chat/completions`, because the two protocols have different request and stream
shapes. The adapter doc comment is
`crates/jcode-provider-openrouter-runtime/src/opencode_go_responses.rs:1-19`; the
model list is `RESPONSES_MODELS` at `:13-21`; the routing predicate is
`model_uses_responses_api` at `:28-42`; the request builder and stream are
`:44-90` and the `ResponsesStream` used from
`crates/jcode-provider-openrouter-runtime/src/openrouter_sse_stream.rs:313`. The
six Responses models are `grok-4.6`, `grok-4.7`, `gpt-5.6-luna`, `gpt-6-luna`,
`muse-spark-1.3-contributor`, `muse-spark-1.2-contributor`.

The predicate accepts either of two independent signals:

```rust
let is_go_profile = profile_id.is_some_and(|id| id.eq_ignore_ascii_case("opencode-go"));
let is_go_endpoint = api_base.to_ascii_lowercase().contains("opencode.ai/zen/go/");
(is_go_profile || is_go_endpoint) && RESPONSES_MODELS.iter().any(...)
```

This is the whole reason the override must change only the base URL and not the
profile identity. A proxy base URL fails `is_go_endpoint` by construction, so
`is_go_profile` has to carry the routing. The sanctioned named-profile route
(`[providers.<name>]` + `--provider-profile`) cannot serve this case: the CLI
helper refuses the reserved name `opencode-go`
(`src/cli/commands/provider_setup.rs:291-311`), and under any other name both
signals fail, so those six models are silently sent to `/chat/completions`.

## Invariants the override must not change

- `profile_id` stays `opencode-go`, preserving `/responses` routing and the route
  label `openai-compatible:opencode-go`
  (`crates/jcode-base/src/provider/mod.rs:236`).
- `display_name` stays "OpenCode Go"; `/model` keeps the same label.
- Static model lists and context limits, which are keyed by profile id
  (`crates/jcode-base/src/provider_catalog.rs:384-400`, `:697-727`).
- `requires_api_key` stays true and the key stays `OPENCODE_GO_API_KEY`, so a
  localhost proxy still needs a (possibly dummy) key.

## Design

An allowlist plus one resolver hook, so "some, not all" is a single-line decision
and the change stays inside `jcode-base`.

```rust
/// Built-in OpenAI-compatible profiles whose endpoint may be redirected to a
/// proxy or self-hosted gateway via `JCODE_<PROFILE_ID>_API_BASE`.
/// Per-profile granularity is deliberate: some profiles select protocol or
/// auth from their base URL (see "Do not allowlist" below).
const API_BASE_OVERRIDABLE_PROFILES: &[&str] = &[
    "opencode",
    "opencode-go",
    "fireworks",
    "minimax",
];

fn api_base_override_env_name(profile_id: &str) -> Option<String> {
    let id = API_BASE_OVERRIDABLE_PROFILES
        .iter()
        .find(|id| id.eq_ignore_ascii_case(profile_id))?;
    Some(format!(
        "JCODE_{}_API_BASE",
        id.to_ascii_uppercase().replace('-', "_")
    ))
}

fn api_base_override(profile_id: &str, env_file: &str) -> Option<String> {
    let name = api_base_override_env_name(profile_id)?;
    load_env_value_from_env_or_config(&name, env_file)
}
```

`load_env_value_from_env_or_config`
(`crates/jcode-provider-env/src/lib.rs:144-167`) reads the process environment
first and then the named env file, which gives two useful properties:

- An override written into `~/.jcode/opencode-go.env` — beside the existing
  `OPENCODE_GO_API_KEY` — is picked up by a long-lived daemon at the next resolve,
  with no restart.
- A process-environment override is fixed at daemon spawn, matching every other
  `JCODE_*` provider variable.

Hook placement: inside `resolve_openai_compatible_profile_endpoint`, after the
key-based heuristic and before the early return, so it applies to all profiles,
beats the inferred MiniMax rule (explicit beats inferred), and is inherited by
every consumer.

```rust
apply_profile_key_based_endpoint_overrides(profile, &mut resolved, api_key_hint); // :79

if let Some(raw) = api_base_override(profile.id, profile.env_file) {
    match normalize_api_base(&raw) {
        Some(base) => resolved.api_base = base,
        None => eprintln!(
            "Warning: ignoring invalid {raw} for provider profile '{}'. Use https://... or http://localhost.",
            profile.id
        ),
    }
}
```

`normalize_api_base` (`crates/jcode-provider-metadata/src/lib.rs:248-268`) already
accepts `https://` plus loopback, private, link-local, `.local` and `.lan` hosts,
and rejects public plain-http, so no new validation code is needed.

Why one insertion is enough — `resolved.api_base` flows to:

- `apply_openai_compatible_profile_env_impl` (`:787-841`), which sets
  `JCODE_OPENROUTER_API_BASE` from it (`:822`) and therefore feeds the runtime.
- `openai_compatible_profile_is_configured` (`:1187`) and the cheap
  `resolve_openai_compatible_profile_endpoint` used by auth-status checks.
- `direct_openai_compatible_profile_routes`
  (`crates/jcode-base/src/provider/mod.rs:213-280`), which renders the base URL in
  the `/model` route detail.
- `crates/jcode-base/src/provider/openrouter.rs:144` and
  `src/cli/auth_test/choice.rs:143-155`, both of which consume resolved profiles.

### Do not allowlist: profiles whose base URL is load-bearing

Four of the 43 built-in profiles select protocol or authentication from their own
base URL, so redirecting them changes behavior beyond the host. The allowlist
exists precisely to keep these out:

| Profile id | Base URL | What breaks if redirected |
| --- | --- | --- |
| `kimi` | `https://api.kimi.com/coding/v1` | `is_kimi_coding_api_base` (`crates/jcode-provider-openrouter-runtime/src/request_headers.rs:7-13`) matches on host `api.kimi.com` plus a `/coding` path; the Kimi coding endpoint gets its own request headers and forced `reasoning_content` replay (`crates/jcode-provider-openrouter-runtime/src/lib.rs:1540`, `openrouter_provider_impl.rs:57-71`) |
| `anthropic-api` | `https://api.anthropic.com/v1` | `api_base_is_anthropic` switches auth to `x-api-key` plus `anthropic-version` (`crates/jcode-base/src/provider_catalog.rs:763-780`); a proxy base silently changes the auth scheme |
| `mistral` | `https://api.mistral.ai/v1` | `strict_openai_schema_endpoint` suppresses non-standard request and message fields when the base contains `mistral.ai` (`crates/jcode-provider-openrouter-runtime/src/lib.rs:1567-1574`) |
| `openrouter` | `https://openrouter.ai/api/v1` | `provider_features_enabled` and the provider-routing/pinning surface are keyed on the `openrouter.ai` substring (`crates/jcode-provider-openrouter-runtime/src/lib.rs:225-235`), so the override would disable or mis-route them |

Two allowlisted cases carry an interaction worth naming:

- `minimax` has a *key*-based endpoint heuristic: an `sk-cp-` prefixed key
  selects the China base (`crates/jcode-base/src/provider_catalog.rs:79`,
  `:254-277`). The hook is placed after that heuristic, so a configured override
  wins over the inferred host. Consequence to document: a user on a China-plan key
  who sets an override will be sent to the override host instead of
  `https://api.minimaxi.com/v1`. This ordering is intentional — an explicit value
  must beat an inferred one.
- `fireworks` has no base-URL-keyed behavior; only its static model list and
  login-provider alias are keyed by profile id
  (`crates/jcode-base/src/provider_catalog.rs:618-625`,
  `crates/jcode-provider-metadata/src/lib.rs:660-661`). Nothing else depends on
  its endpoint.

The two remaining unlisted cases are harmless but pointless: `lmstudio` and
`ollama` already point at `http://localhost:...`
(`crates/jcode-provider-metadata/src/catalog.rs:366`, `:377`).

## Accepted cost: the model-catalog cache namespace is shared

Decision taken in review: keep `JCODE_OPENROUTER_CACHE_NAMESPACE = opencode-go`
and share the cache file. The disk cache is keyed by namespace
(`crates/jcode-provider-openrouter/src/lib.rs:342-370`) and records
`source_api_base` (written at
`crates/jcode-provider-openrouter-runtime/src/catalog_fetch.rs:89`); consumers
compare that against the resolved base and reject a mismatch
(`crates/jcode-base/src/provider/mod.rs:196`, `crates/jcode-base/src/provider_catalog.rs:147-150`,
`crates/jcode-provider-openrouter-runtime/src/lib.rs:1939`).

Consequences to document for users:

- Immediately after switching to a proxy, the cached catalog is rejected and
  `/model` falls back to the static list, marked `…; fallback: static provider
  model list` (`crates/jcode-base/src/provider/mod.rs:240-245`).
- If the proxy answers `/models`, that response is written to
  `opencode-go_models.json` with `source_api_base = <proxy>`; switching back to
  the real endpoint rejects it once and re-fetches. Degrade, then self-heal — not
  corruption.
- The static fallback list for `opencode-go`
  (`crates/jcode-base/src/provider_catalog.rs:392-400`) does not contain the six
  Responses models, so a proxy that serves no `/models` will not offer them in
  `/model` at all.

## Non-goals

- Non-OpenAI-compatible providers. Anthropic, OpenAI, Gemini and Copilot each have
  their own override variable and their own resolver
  (`JCODE_ANTHROPIC_API_BASE` at `crates/jcode-base/src/provider_catalog.rs:859`,
  `JCODE_OPENAI_API_BASE` at `crates/jcode-base/src/provider/openai.rs:31`).
- Per-model base URLs; that needs a new field on `NamedProviderModelConfig`
  (`crates/jcode-config-types/src/lib.rs:446-471`) plus runtime plumbing.
- A `[provider]`/`[providers]` config.toml field. The named-profile route already
  covers "point this at my own gateway" for anyone who does not need OpenCode Go
  identity; this plan exists only for the identity-preserving case.
- Relaxing `normalize_api_base` to allow public plain-http hosts.
- Request rewriting, failover or retry behavior inside the proxy.

## Review decisions (2026-10-15)

1. **Cache namespace is shared.** No change to
   `JCODE_OPENROUTER_CACHE_NAMESPACE`; the accepted costs above apply.
2. **No login wiring.** The override is environment- or env-file-only. The
   base-URL prompt stays gated on `is_custom_profile`
   (`src/cli/login.rs:834-878`) and the TUI prompt
   (`crates/jcode-tui/src/tui/app/auth.rs:2617-2645`) is untouched. Documented,
   not prompted.
3. **The cost-display locality gap stays.** `crates/jcode-base/src/model_pricing.rs:229`
   is recorded as a known cosmetic gap rather than fixed in this change. It
   affects cost display only; no request routing or authentication reads it.
4. **Allowlist contents: `["opencode", "opencode-go", "fireworks", "minimax"]`.**
   Chosen explicitly rather than by exclusion rule. The four base-URL-load-bearing
   profiles above stay out, and everything else stays out until someone asks:
   widening the list later is a one-line edit, and nothing changes for a user who
   does not set the override variable.

No open decisions remain.

## Audit: every read of the raw `api_base` constant

| Site | Honours override | Impact if not fixed |
| --- | --- | --- |
| `provider_catalog.rs:822` | Yes (resolved) | Runtime base URL |
| `provider/openrouter.rs:144` | Yes (resolved) | Autodetected profile base |
| `src/cli/auth_test/choice.rs:143-155` | Yes (resolved param) | `auth-test opencode-go` probes the override target |
| `provider_catalog.rs:315` | No (constant) | `active_openai_compatible_display_name` misses the base→name lookup, but the namespace check at `:294-303` runs first, so cosmetic |
| `provider_catalog.rs:355` | No (constant) | `openai_compatible_profile_id_for_api_base` fallback misses; the runtime prefers the namespace (`runtime lib.rs:1585-1589`), so cosmetic |
| `model_pricing.rs:229` | No (constant) | A localhost proxy is still priced as a hosted reseller: unknown model ids may borrow another provider's prices |

## Phases

### Phase 1 — Resolver hook and allowlist

**T step.** Add to `crates/jcode-base/src/provider_catalog_tests.rs` (reuse the
`EnvGuard` pattern at `:1045`), run `cargo test -p jcode-base provider_catalog`,
and confirm each new case fails before the S step:

1. `every_allowlisted_profile_honours_api_base_override` — parametrised over all
   four ids (`opencode`, `opencode-go`, `fireworks`, `minimax`): with
   `JCODE_<ID>_API_BASE=https://proxy.example/v1`,
   `resolve_openai_compatible_profile(<PROFILE>)` yields that base while `id`,
   `api_key_env` and `requires_api_key` are unchanged. For `opencode-go`
   specifically this pins that `/responses` routing keeps its profile-id signal.
2. `non_allowlisted_profile_ignores_api_base_override` — `JCODE_ZAI_API_BASE` set
   leaves ZAI on its constant.
3. `minimax_api_base_override_outranks_key_heuristic` — with an `sk-cp-` key and
   no override, `minimax` resolves to `MINIMAX_CHINA_API_BASE`; with
   `JCODE_MINIMAX_API_BASE` also set, it resolves to the override. This is the
   ordering decision from Review decision 4.
4. `invalid_api_base_override_is_ignored` — non-URL values and public
   `http://host` values leave the constant in place.
5. `blank_api_base_override_is_ignored` — whitespace-only value.
6. `api_base_override_env_name_is_stable` — for all four ids, the derived name is
   `JCODE_<ID>_API_BASE` with `-` mapped to `_`.

**S step.** In `crates/jcode-base/src/provider_catalog.rs` add
`API_BASE_OVERRIDABLE_PROFILES`, `api_base_override_env_name` and
`api_base_override`, and insert the hook between `:79` and `:81`. Extend
`tracked_env_vars()` in `crates/jcode-base/src/auth/test_sandbox.rs:102-153` so the
derived override names for allowlisted profiles are saved and cleared alongside
the existing `profile.api_key_env` injection, otherwise sandboxed auth tests can
leak an override from the ambient environment.

### Phase 2 — Identity and `/responses` routing survive the override

**T step.** In the tests module of
`crates/jcode-provider-openrouter-runtime/src/opencode_go_responses.rs`:

1. `proxy_base_keeps_responses_routing_via_profile_id` —
   `model_uses_responses_api("https://proxy.example/v1", Some("opencode-go"), "grok-4.7")`
   is true.
2. `proxy_base_without_go_identity_uses_chat_completions` — same base,
   `Some("opencode-go-proxy")`, is false. This is the regression the design
   exists to prevent.

And in `crates/jcode-provider-openrouter-runtime/src/openrouter_tests.rs`, using
its existing `EnvVarGuard`/`ENV_LOCK` harness (see
`named_openai_compatible_provider_sets_catalog_cache_namespace` at `:332-355`):

3. `proxy_base_still_resolves_the_go_profile_id` — with
   `JCODE_OPENROUTER_API_BASE=https://proxy.example/v1`,
   `JCODE_OPENROUTER_CACHE_NAMESPACE=opencode-go` and
   `JCODE_OPENROUTER_API_KEY_NAME=OPENCODE_GO_API_KEY` set, the constructed
   provider reports api base `<proxy>` and profile id `opencode-go`.

**S step.** Expected: none. If case 3 fails, confirm the namespace-first
derivation at `crates/jcode-provider-openrouter-runtime/src/lib.rs:1585-1595` and
investigate rather than patching.

### Phase 3 — Documentation

**T step.** Documentation only; verified by review. No test file changes.

**S step.** Document the variable in `docs/AUTH_CREDENTIAL_SOURCES.md` together
with the accepted costs above, the shared-namespace consequence, and the fact
that the provider key is sent to whatever host the override names. No login-flow
or TUI prompt changes (Review decision 2).

## Validation matrix

- `cargo test -p jcode-base provider_catalog`
- `cargo test -p jcode-base provider` — route/label tests at
  `crates/jcode-base/src/provider/tests/model_resolution.rs:748-803` assert
  `openai-compatible:opencode-go`
- `cargo test -p jcode-provider-openrouter-runtime`
- Unchanged-by-default check: with no override in the environment and no override
  in the env file, `resolve_openai_compatible_profile(OPENCODE_GO_PROFILE).api_base`
  equals `OPENCODE_GO_PROFILE.api_base`.
- Manual, env route: `JCODE_OPENCODE_GO_API_BASE=http://localhost:9000/v1 jcode -p opencode-go`,
  then with `--trace` confirm the request host is the proxy, `grok-4.7` goes to
  `/responses`, and `kimi-k2.5` goes to `/chat/completions`.
- Manual, file route: write the same variable into `~/.jcode/opencode-go.env`,
  start a new session without restarting the daemon, confirm the proxy is used.

## Risks

- Silent misrouting is the failure mode: if `profile_id` ever stops being
  `opencode-go` while the base is a proxy, the six Responses models are sent to
  `/chat/completions`. Phase 2 pins this.
- An override whose host does not proxy `/models` hides the Responses models from
  `/model`, because the static fallback list does not contain them.
- The provider API key is sent to whatever host the override names. The override
  is opt-in and user-owned, but the invalid-value warning should say so, and the
  docs line should repeat it.
