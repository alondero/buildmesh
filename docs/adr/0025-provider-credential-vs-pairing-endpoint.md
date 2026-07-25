# 25. Provider credentials vs pairing endpoints

Status: accepted (amends ADR-0016)

Providers hold credentials and billing only; harness pairings hold endpoint URL and model tiers. Keyed first-class providers start absent until added; saving a key never auto-attaches a pairing.

## Context

ADR-0016 put API key on the provider and surface/URL/tiers on the pairing, then amended Anthropic resolution to re-derive URL/tiers from the account because the UI only edited those fields on Providers. That left the UI fighting the model: base URL and Claude model aliases lived on the provider card even when the user only used Codex, keyed first-class rows (MiniMax, Kimi, OpenRouter) were pre-seeded empty, and a key save silently derived a Claude pairing.

## Decision

1. **Providers page fields.** Self-auth first-class rows (Anthropic, Codex, Antigravity, Grok, OpenCode) always appear — enable + billing mode (OpenCode keeps its OAuth card). Keyed first-class providers are absent until added from a catalog. Generics are name + API key only. No base URL or model tiers on any provider card.
2. **Harnesses page owns pairing config.** Attach (and later inline edit) always collects base URL (first-class prefilled from `first_class_surfaces`). Anthropic-surface pairings also collect the model-tier remap. Surface is implied by the target harness. Only **stored** pairings exist — no derived default Anthropic pairing on key save.
3. **One-shot migration.** On prefs load, once (`ad0025_account_pairings_migrated`), for each enabled keyed account without a Claude pairing, materialise one from legacy account `base_url`/`model_tiers` or first-class defaults, then strip those fields from accounts. Subsequent loads never auto-attach.
4. **Generic multi-surface.** A generic credential may attach under any proxy-capable harness; surface and URL are per pairing (supersedes "generic declares exactly one surface on the account").
5. **Spawn path.** Composite spawn env resolves only stored pairings — no first-class synthesis without an explicit attach. Attach defaults (`pairing_for` / `get_pairing_defaults`) may still prefill from `first_class_surfaces`.

## Consequences

- `effective_pairings` is stored pairings only.
- Anthropic spawn resolution honours the stored pairing payload again (reverts the "always re-derive from account" amendment in ADR-0016).
- Spawn menu proxied rows appear only after explicit attach under Harnesses.
