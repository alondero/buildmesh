# ADR-0051: The pairing verification warns; it does not gate the launch

Status: accepted
**Date:** 2026-10-31

## Context

A Codex proxy route can carry a *pairing verification*: a proof, for one exact
`(harness, provider, endpoint, model, credential, runtime, Codex build)`, that
the Responses agent loop answered. The proof is a signature over that tuple,
and the installed Codex build is part of it.

That makes the proof go stale on its own. A routine `codex-cli` npm update
changes the installed version, the signature no longer matches, and the record
turns `Stale` — with the reason "the Codex CLI changed"
(`stale_after_cli_update_reports_the_cli_change_not_routing` in
`services::provider_verification_tests` pins that transition). The route was
verified; the *inputs it was verified against* moved.

The launch path treated that state as a gate. `verified_codex_pairing` refused
to build a route unless the record's status was `Verified`, so the spawn
preflight failed with "pairing 'MiniMax' is not launchable: the Codex CLI
changed (0.166.0, previously 0.165.0); verify the pairing again". The Spawn
Menu went further: an unverified route was filtered out of the derived menu
entirely, and the routing catalog marked OpenAI-surface routes unavailable
("Runtime verification pending").

The effect was a launch that could not be started at all, for a route the user
had configured and that might well work — followed by a manual "Verify
pairing" click in Settings. With the CLI auto-updating, that friction recurs
on a schedule the user does not control.

## Decision

**The pairing verification is advisory at launch.** A route that is unchecked,
stale or previously failed still launches; the session runs.

What still fails closed is the route's *completeness*, and only that:

- the provider account is disabled;
- the account has no credential;
- the endpoint does not resolve to a valid HTTPS URL;
- the capability contract is declared incompatible for this provider/surface;
- no Codex install exists for the runtime.

Those are the failures trying again cannot fix. Each keeps its own reason.

The verification state travels with the route as a `VerificationAdvisory`
(`Missing`, `Stale { reason }`, `Failed { reason }`) and buys three things:

- the Spawn Menu row carries `verification_warning` (`ProviderInfo`), naming
  the reason next to the route, and the row stays selectable;
- a launch on such a route emits the `provider-warning` event
  (`ProviderWarningPayload`), which the desktop surfaces as a warning toast —
  the session started, and here is what is uncertain about it;
- an unchecked or stale route schedules a background pairing verification while
  the session runs, so the next launch finds a current record instead of
  asking the user to re-verify by hand.

A *failed* check is not re-probed on every launch. It needs a credential or
endpoint fix first, and probing it again would only repeat a request the
endpoint has already rejected. A credential change makes the signature stale,
which is a re-checkable state again.

The verification itself is unchanged: the same probe, the same signature, the
same record, the same Settings UI. Only its authority over the launch changed.

## Alternatives

*Keep the gate and tell the user to re-verify.* Rejected: it makes the CLI's
update cadence a source of launch failures, and the user cannot act on the
failure before it happens — only after.

*Warn and block for a known-failed check, relax only the unchecked and stale
cases.* Rejected in review: a failed check is a statement about one moment
against one endpoint; the user asked for it to be a warning, and the warning
carries the reason.

*Auto-re-verify on a timer, independent of launches.* Rejected: it spends
requests against provider endpoints the user may not be using. Re-checking when
the user actually launches spends the request where it has value.

*Drop the pairing verification entirely.* Rejected: the proof still tells the
user something real — that this combination worked once, on this build — and
the Settings pane still needs it to show what was checked.

## Consequences

The Codex proxy route no longer carries a guaranteed-verified record, so
`PreparedLaunchRouting::CodexProxy::verification` is now an `Option` and the
route carries an `advisory` beside it. `codex_proxy_route` and
`codex_proxy_route_snapshot` replace `verified_codex_pairing` and
`verified_codex_snapshot`; the names encoded the old policy and would have
misled the next reader.

`launchable_on_runtime_with` no longer reads the verification records at all —
launchability is the route's completeness. The records drive
`route_advisory_with` instead, which is what the menu renders.

The launch path gains a warning event and a background verification schedule.
The scheduled verification is de-duplicated per `(harness, provider, runtime)`
so two launches in quick succession do not queue two probes against the same
endpoint, and the de-duplication entry is released by a drop guard — a
panicking probe must not leave the route permanently un-re-checkable, because
the next launch would then silently stop verifying.

A route that has never been checked now launches with a warning on every
launch until a background check succeeds. The check runs in parallel with the
session, so the window is short; a route the endpoint rejects keeps warning
with the rejection reason, which is the information the user needs to fix it.

The Settings routing catalog does not carry the warning. It resolves routes
without probing the Codex CLI — the same constraint that keeps that pane cheap
— so it has no install to check the record against. The Spawn Menu and the
launch itself are where the warning belongs, and both have it.

## Verification

The behaviour is pinned by these tests:

- `services::provider_verification::tests::an_unchecked_route_launches_with_a_missing_advisory`
- `services::provider_verification::tests::a_stale_route_after_a_cli_update_launches_and_names_the_cli_change`
- `services::provider_verification::tests::a_route_whose_check_failed_launches_and_keeps_the_failure_reason`
- `services::provider_verification::tests::a_route_without_a_credential_or_with_a_disabled_account_still_fails_closed`
- `preferences::launch_configurations::tests::an_unverified_openai_route_still_resolves_because_the_check_only_warns`
- `preferences::launch_configurations::tests::a_stale_verification_is_carried_rather_than_refused`
- `agent::launch_routing::launch_routing_tests::an_unchecked_route_warns_and_asks_for_a_background_recheck`
- `agent::launch_routing::launch_routing_tests::a_stale_route_warns_and_asks_for_a_background_recheck`
- `agent::launch_routing::launch_routing_tests::a_failed_route_warns_without_asking_for_a_recheck`
- `agent::provider_menu::tests::an_unverified_proxied_row_carries_the_warning_instead_of_disappearing`
- `commands::preferences::preferences_tests::a_second_probe_for_the_same_route_is_refused_while_the_first_is_in_flight`
- `tests/unit/grouped-provider-menu.test.tsx` — the warning renders and the row
  stays selectable

See [Providers](../development/providers.md#the-pairing-verification-warns-it-does-not-gate)
for the current behaviour.
