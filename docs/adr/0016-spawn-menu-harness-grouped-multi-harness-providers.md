# 16. Spawn Menu: harness-grouped, ordered, multi-harness proxied providers

Status: accepted (refines ADR-0014)

## Context

ADR-0014 separated **Agent Harness** (executor) from **Model Provider** (credentials/endpoint) and specified that "the UI launch dropdown renders user-configured profiles" — i.e. a **flat list of harness×provider profiles** ("MiniMax via Claude Code" as a row), with a configured provider auto-paired to the Claude Code harness *only* (#537).

A UX design session (originating from a request to drag-reorder providers) found that flat shape under-serves the model:

- It conflates two orthogonal axes in one "Accounts & Usage" panel — a *provider* axis (credentials + usage) and a *harness* axis (executors + ordering) — and hangs a reorder action off a usage readout.
- It can't express a **Proxied Provider** reaching *multiple* harnesses (MiniMax via Claude Code over its Anthropic-compatible surface *and* via Codex over its OpenAI-compatible surface).
- It double-counts usage for a provider proxied through two harnesses, and has no place for a harness-native subscription's usage (Claude Code's subscription quota, which Buildmesh already surfaces with no API key).

The domain terms this introduces (Proxied/Native Provider, Compatible API surface, First-class/Generic Model Provider, Usage Meter, Spawn Option, Spawn Menu) are defined in `CONTEXT.md`.

## Decision

1. **Spawn Menu shape — harness-grouped, always-expanded flat list.** Each **Agent Harness** is *one clickable row* that launches the harness natively (Claude Code→Anthropic, Codex→OpenAI, OpenCode→its own login, Terminal→a shell). Its **Proxied Providers** appear as always-visible indented child rows; each row is a one-click **Spawn Option**. No nested/hover submenus (so the header is a pure launch action, never dual-action) and no click-to-collapse. `Terminal` is pinned last.

2. **One backend-derived Spawn Menu, rendered as-is everywhere.** Sidebar, Issues probe, PRs probe, archived-resume, and mobile all render the same ordered, grouped menu; none re-orders or re-derives it. Mobile is a read-only reflection (no mobile reorder/config).

3. **Harness-level ordering, persisted, detection-safe.** An ordered list of harness ids in `preferences.json`; `Terminal` always forced last. A newly-**detected** harness is appended at the end (above Terminal); an **uninstalled** harness keeps its slot (re-rendered if reinstalled) but is hidden while absent. Provider-level (child) reordering is **deferred** to a follow-up issue.

4. **Multi-harness proxied attach, config split by scope.** A Model Provider may be proxied through any harness whose **Compatible API surface** it exposes. The **API key is global** to the provider (one canonical editor on the Providers page; reused across pairings). The **surface + endpoint URL + model-tier remap are per harness×provider pairing**. A First-class provider publishes its surface→URL map (pairing names the surface); a **Generic provider declares exactly one surface+URL** (multi-surface custom endpoints become two entries).

5. **Two pages.** A provider-centric **Providers** page owns credentials (canonical key editor) and renders **Usage Meters**; a harness-centric **config** page reorders harnesses and attaches proxied providers. Usage is **detection-gated**: a harness-native subscription meter shows when the harness is installed (no key needed); a keyed provider's meter shows when its key is set; an uninstalled harness's meter is never shown. A provider may have **more than one Usage Meter** (e.g. Anthropic subscription *and* API wallet); proxying one credential through many harnesses is still one meter.

6. **Spawn Option id + one-off migration.** Native option id = `<harness_id>` (e.g. `claude`, `codex`, `terminal`); proxied option id = `<harness_id>:<provider_id>` (e.g. `claude:minimax`, `codex:minimax`). Resolution splits on `:` → executor from the harness part, creds+surface from the provider part. Because `agent_nodes` rows persist indefinitely (close = `status='archived'`, resumable), legacy bare ids would otherwise need a permanent resolver shim — so a **one-off `SCHEMA_VERSION` migration** rewrites stored legacy ids (`minimax`/`kimi`/custom bare account id → `claude:<id>`) in place. The mapping is unambiguous today because every proxied provider currently pairs with Claude Code only; doing it now (before multi-harness attach ships) keeps it one-to-one.

## Considered Options

- **Flat profile list (ADR-0014 original)** — one row per pairing. One-click and simple, but grows combinatorially, gives no harness structure, and was the source of the conflation above.
- **Nested/hover submenus** (the original request) — compact top level, but adds a click to non-default combos (penalising the *most common* path, e.g. Claude Code→MiniMax), reintroduces the dual-action parent, and is awkward on touch.
- **Grouped-flat (chosen)** — every combo one click *and* harness structure, no nesting cost, mobile-trivial; folds the original reorder request in as the favourites mechanism.
- **Resolver shim vs one-off migration for legacy ids** — chose migration because archived nodes persist forever, so a shim is a permanent maintenance burden, and the legacy→composite mapping is unambiguous only while proxied providers pair with Claude Code alone.

## Consequences

- `ProviderInfo` becomes the **Spawn Option** wire type, gaining `harness_id`, optional `provider_id`, `is_proxied`, and grouping/order metadata (ts-rs regenerated, committed, drift-gated per ADR-0009).
- `resolve_provider_env` becomes pairing-scoped/surface-aware (it must pick the right base URL + model-tier map for the harness's surface, not a single per-account base URL).
- Subsumes the menu/usage portions of PRD #566 (first-class providers) and extends #567 (per-tier model map) to be pairing-scoped.
- The "Accounts & Usage" modal splits into the Providers page + harness config page.

### Implementation notes (issue #576)

- The per-pairing split is realised by `preferences::ProviderPairing { harness_id, provider_id, surface, base_url, model_tiers }` and an `ApiSurface { Anthropic, OpenAI }` enum. Only **user-added** pairings are persisted (`AppPreferences::provider_pairings`); the default Anthropic pairing for a keyed account is **derived** at read time (`effective_pairings`), so pre-#576 MiniMax-via-Claude setups keep working with **no migration** — a stored pairing for the same `(harness_id, provider_id)` overrides the derived default.
- First-class providers publish their surface→URL(+default model) map in `preferences::first_class_surfaces` (the "publishes its surface→URL map" mechanism); a Generic provider declares its single Anthropic surface+URL on the account. `resolve_provider_env` dispatches by surface: `anthropic_surface_env` (`ANTHROPIC_*` + per-tier aliases) vs `openai_surface_env` (`OPENAI_BASE_URL`/`OPENAI_API_KEY`/`OPENAI_MODEL`).
- **The OpenAI/Codex surface is wired best-effort and not yet runtime-verified** (no `codex` binary + provider OpenAI key on the dev host). The Anthropic surface is exercised end-to-end. Live Codex verification — including whether Codex honours `OPENAI_*` env vs a `~/.codex/config.toml` `model_providers` entry — is tracked in **#599**.
