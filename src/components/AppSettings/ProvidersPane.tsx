/**
 * `ProvidersPane` — provider routing defaults (issue #1880): which
 * provider a new agent spawns on, which one runs background classification
 * and auto-naming, and which one the adversarial review circuit uses.
 *
 * Each picker saves immediately and is therefore never a dirty site — there
 * is no half-typed state to discard, so an accidental close cannot lose
 * anything. What they *do* have is optimistic rollback: the value flips
 * locally, the IPC runs, and a failure restores the previous value.
 *
 * The rollback reads `previous` from a ref rather than the closure
 * (issue #581): a closure-captured snapshot goes stale the moment a
 * re-render commits a new value, so two changes fired in quick succession
 * would both roll back to the same stale value instead of to the value as
 * of their own selection.
 *
 * Credential editing (the account cards and the add-provider form) lives
 * in `AccountsPane`.
 */
import { useCallback, useRef, useState } from 'react';
import * as api from '../../lib/tauri';
import type { AppPreferences } from '../../types/generated/AppPreferences';
import { formatError } from '../../lib/errorUtils';
import { backgroundInferenceOption } from '../../lib/backgroundInference';
import { SpawnOptionPicker } from '../Providers/SpawnOptionPicker';
import { blocksReviewCircuit } from '../Circuits/harnessCapabilities';
import { SettingsRow, SettingsSection } from './SettingsRow';
import { ResourceLoadStatus } from './ResourceLoadStatus';
import { NO_OVERRIDE } from './settingsUtils';
import { useSettingsData } from './SettingsDataContext';

export function ProvidersPane() {
  const {
    resources,
    prefsLoaded,
    preferences,
    providers,
    routingProviders,
    loadPreferences,
    retryResource,
    setError,
  } = useSettingsData();

  const [selected, setSelected] = useState<string>(NO_OVERRIDE);
  const [reviewerProvider, setReviewerProvider] = useState<string>(NO_OVERRIDE);
  const [saving, setSaving] = useState(false);
  const [reviewerSaving, setReviewerSaving] = useState(false);
  const [classifierProvider, setClassifierProvider] = useState<string | null>(null);
  const [classifierSaving, setClassifierSaving] = useState(false);
  // Issue #824: the user-configured rename backend. `null` means
  // auto-naming is OFF (the post-v2 default). Distinct from `selected`
  // above (default provider for spawn), since rename runs frequently on
  // trivial content and shouldn't inherit the node's model.
  const [namingProvider, setNamingProvider] = useState<string | null>(null);
  const [namingSaving, setNamingSaving] = useState(false);
  // The preferences payload whose picker values are currently rendered.
  const [adoptedPrefs, setAdoptedPrefs] = useState<AppPreferences | null>(null);

  // Rollback baselines, one per picker, in a single shape so all four
  // handlers share one convention (issue #581). Each ref is written
  // SYNCHRONOUSLY inside its own handler at selection time, before the await.
  // A rapid second change therefore rolls back to the value as of its own
  // selection rather than to whatever a stale closure captured — which is
  // what an effect-mirrored ref or a `const previous = state` closure gives
  // you when two writes overlap.
  const pickers = useRef({
    selected: NO_OVERRIDE as string,
    reviewer: NO_OVERRIDE as string,
    classifier: null as string | null,
    naming: null as string | null,
  });

  // Seed the four pickers from the latest winning preferences read *during
  // render* (React's documented adjust-state-while-rendering pattern), so a
  // picker is never rendered enabled while still showing the pre-load value.
  // Each payload is a distinct object, so identity is a sufficient "this load
  // landed" signal.
  //
  // The baselines are re-seeded here too, because a committed load is the
  // authoritative value every later rollback should target. Writing them in
  // render alongside the state they mirror is deliberate: they are this
  // component's own derived value, not an external side effect, and the
  // handlers that read them all run after commit.
  if (preferences && preferences !== adoptedPrefs) {
    setAdoptedPrefs(preferences);
    const stored = preferences.default_provider;
    const nextSelected = stored && stored.length > 0 ? stored : NO_OVERRIDE;
    const storedReviewer = preferences.reviewer_provider;
    const nextReviewer = storedReviewer && storedReviewer.length > 0 ? storedReviewer : NO_OVERRIDE;
    const nextClassifier = preferences.circuit_classifier_provider ?? null;
    const storedNaming = preferences.naming_provider;
    const nextNaming = storedNaming && storedNaming.length > 0 ? storedNaming : null;
    pickers.current = {
      selected: nextSelected,
      reviewer: nextReviewer,
      classifier: nextClassifier,
      naming: nextNaming,
    };
    setSelected(nextSelected);
    setReviewerProvider(nextReviewer);
    setClassifierProvider(nextClassifier);
    setNamingProvider(nextNaming);
  }

  // Per-resource readiness booleans. Controls that read or write a
  // resource must gate themselves on its `loaded` state — `idle`,
  // `loading`, and `failed` all disable, so a placeholder initial value
  // (`false` / `[]` / `{}`) can never be written to the backend as if
  // it were the real persisted state.
  const providersLoaded = resources.providers.status === 'loaded';
  const routingLoaded = resources.routing.status === 'loaded';
  const routingChoices = providersLoaded ? providers : routingProviders;
  const routingReady = providersLoaded || routingLoaded;

  // Persist the default-provider dropdown.
  const handleSave = async (newValue: string) => {
    const previous = pickers.current.selected;
    pickers.current.selected = newValue;
    setSelected(newValue);
    setSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await api.setAppDefaultProvider(providerArg);
      // Issue #1534 (review round 5) — refresh via the loader so a
      // failed `get_app_preferences` (or a backend normalisation)
      // flips `prefsState` to `failed` and surfaces the banner
      // rather than leaving the optimistic value lying.
      await loadPreferences();
    } catch (e) {
      pickers.current.selected = previous;
      setSelected(previous);
      setError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  // The reviewer default is deliberately a separate preference from the
  // ordinary spawn default. Reviews are often adversarial, so the useful
  // configuration is "use this independent Spawn Option" while preserving
  // the source-agent fallback when the selector is cleared.
  const handleSaveReviewer = async (newValue: string) => {
    const previous = pickers.current.reviewer;
    pickers.current.reviewer = newValue;
    setReviewerProvider(newValue);
    setReviewerSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await api.setAppReviewerProvider(providerArg);
      await loadPreferences();
    } catch (e) {
      pickers.current.reviewer = previous;
      setReviewerProvider(previous);
      setError(formatError(e));
    } finally {
      setReviewerSaving(false);
    }
  };

  // Issue #824: persist the rename backend. Distinct from `handleSave`
  // above — auto-naming runs frequently on trivial content, so it lives
  // on its own picker. Empty string is normalised to `null` so the picker
  // value reads as "auto-naming off" rather than as some bizarre empty id.
  const handleSaveNaming = async (newValue: string | null) => {
    const previous = pickers.current.naming;
    const next = newValue && newValue.length > 0 ? newValue : null;
    pickers.current.naming = next;
    setNamingProvider(next);
    setNamingSaving(true);
    setError(null);
    try {
      await api.setAppNamingProvider(next);
      // Issue #1534 (review round 5) — backend normalises empty
      // string to `null` (and may reject unknown providers), so a
      // post-write refresh via the loader is what catches a backend
      // reject / normalisation that the optimistic update missed.
      await loadPreferences();
    } catch (e) {
      pickers.current.naming = previous;
      setNamingProvider(previous);
      setError(formatError(e));
    } finally {
      setNamingSaving(false);
    }
  };

  // The classifier picker is optimistic like the other three. It previously
  // waited for the IPC before flipping the visible value, which made the
  // control look unresponsive and left it showing a value the backend had
  // just rejected. Same rollback contract as its siblings.
  const handleSelectClassifier = useCallback(
    (next: string | null) => {
      const previous = pickers.current.classifier;
      pickers.current.classifier = next;
      setClassifierProvider(next);
      setClassifierSaving(true);
      setError(null);
      void api
        .setCircuitClassifierProvider(next)
        .catch((cause: unknown) => {
          pickers.current.classifier = previous;
          setClassifierProvider(previous);
          setError(formatError(cause));
        })
        .finally(() => setClassifierSaving(false));
    },
    [setError],
  );

  return (
    <>
      {/* Routing controls below are preferences + providers backed; surface a
          failure for either before the controls that depend on them. */}
      {resources.preferences.status === 'failed' && (
        <ResourceLoadStatus
          resource="preferences"
          state={resources.preferences}
          onRetry={() => retryResource('preferences')}
        />
      )}
      {/* Providers shows `loading` too, not just `failed` — unlike
          preferences (a cached preferences read, effectively instant) this
          resource is gated on the Codex install probe chain, which can take
          several seconds on a cold WSL distro. The three pickers below are
          disabled until it resolves, so with a failure-only banner the pane
          showed a wall of dead controls and no reason why. Same
          `!loaded` guard the Remote Access pane uses for `network`. */}
      {!providersLoaded && (
        <ResourceLoadStatus
          resource="providers"
          state={resources.providers}
          onRetry={() => retryResource('providers')}
        />
      )}

      {!routingReady && <ResourceLoadStatus resource="routing" state={resources.routing} onRetry={() => retryResource('routing')} />}
      {!providersLoaded && routingLoaded && <p className="mb-3 text-sm text-text-secondary">Saved routing choices are ready. Runtime checks are pending; routes needing verification stay unavailable. Confirm harness login before launching.</p>}
      <SettingsSection
        title="Provider defaults"
        description={
          <>
            Provider defaults are stored in your app data directory at{' '}
            <span className="font-mono">preferences.json</span>; coordinator
            settings and authorized devices live in the app database.
          </>
        }
      >
        <SettingsRow
          label="Default provider"
          htmlFor="default-provider"
          summary="Provider used when a mesh has no default of its own."
        >
          <SpawnOptionPicker
            id="default-provider"
            size="md"
            ariaLabel="Default provider"
            providers={routingChoices}
            value={selected === NO_OVERRIDE ? null : selected}
            unsetLabel="Anthropic (built-in default)"
            unsetValue={NO_OVERRIDE}
            disabled={!prefsLoaded || !routingReady || saving}
            onSelect={next => handleSave(next ?? NO_OVERRIDE)}
          />
        </SettingsRow>

        <SettingsRow label="Circuit classifier provider" htmlFor="circuit-classifier-provider"
          summary="Background model used to classify Circuit agent reports."
          details={<>Independent of agent spawn defaults. Choose a host-native harness with background inference support; its model and effort settings apply. Configurations with extra CLI arguments cannot run background inference.</>}>
          <SpawnOptionPicker id="circuit-classifier-provider" size="md" ariaLabel="Circuit classifier provider" unsetValue={null}
            providers={routingChoices} value={classifierProvider} unsetLabel="Claude Code (built-in default)"
            disabled={!prefsLoaded || !routingReady || classifierSaving}
            filter={(option) => option.harness_id !== 'terminal'}
            decorate={backgroundInferenceOption}
            onSelect={handleSelectClassifier} />
        </SettingsRow>

        <SettingsRow
          label="Reviewer provider"
          htmlFor="reviewer-provider"
          summary="Provider for the built-in adversarial review circuit."
          details={
            <>
              Used by the built-in review circuit for adversarial review. Leave it on
              the source-agent fallback to use the reviewed agent&apos;s provider.
              Authored Circuits can still override this in their reviewer node.
            </>
          }
        >
          <SpawnOptionPicker
            id="reviewer-provider"
            size="md"
            ariaLabel="Reviewer provider"
            providers={routingChoices}
            value={reviewerProvider === NO_OVERRIDE ? null : reviewerProvider}
            unsetLabel="Source agent provider"
            unsetValue={NO_OVERRIDE}
            disabled={!prefsLoaded || !routingReady || reviewerSaving}
            filter={(option) => option.harness_id !== 'terminal'}
            // A reviewer whose harness cannot yield a turn never lets the
            // `verdict` gate fire, so it is offered but not pickable.
            decorate={(option) =>
              blocksReviewCircuit(option.harness_id)
                ? { ...option, unavailable_reason: 'no review support' }
                : option
            }
            onSelect={next => handleSaveReviewer(next ?? NO_OVERRIDE)}
          />
        </SettingsRow>

        {/* Issue #824: Auto-naming. Distinct from the default provider above.
            Runs frequently on trivial content, so the user explicitly opts in
            via this picker; empty / "Disabled" leaves nodes with their random
            adj-adj-noun slugs. */}
        <SettingsRow
          label="Auto-naming"
          htmlFor="auto-naming"
          summary="Small LLM that renames nodes from their work."
          details={
            <>
              When a node finishes a turn, Buildmesh can ask a small LLM to summarise
              the work into a slug (e.g. <code>fix-auth-flow</code>) instead of the
              default <code>bold-keen-brook</code>. Auto-naming runs frequently on
              trivial content — pick a cheap backend so an Opus-class node doesn&apos;t
              burn tokens on every rename.
            </>
          }
        >
          <SpawnOptionPicker
            id="auto-naming"
            size="md"
            ariaLabel="Auto-naming"
            providers={routingChoices}
            value={namingProvider ?? null}
            unsetLabel="Disabled (auto-naming off)"
            unsetValue={null}
            disabled={!prefsLoaded || !routingReady || namingSaving}
            filter={(option) => option.harness_id !== 'terminal'}
            onSelect={next => handleSaveNaming(next)}
            decorate={backgroundInferenceOption}
          />
        </SettingsRow>
        {namingProvider === 'anthropic' && (
          <p className="pb-2 text-sm text-text-muted">
            Built-in Anthropic is pinned to a haiku tier so the rename doesn&apos;t
            inherit your main subscription default.
          </p>
        )}
        {namingProvider === null && (
          <p className="pb-2 text-sm text-text-muted">
            Auto-naming is off. New nodes keep random adjective-adjective-noun slugs.
            You can always rename manually from the sidebar.
          </p>
        )}
      </SettingsSection>
    </>
  );
}