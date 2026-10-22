/**
 * Loads the backend Spawn Menu for a phone surface (issue #2385).
 *
 * The menu is one backend list (ADR-0016): no client re-sorts it. The phone
 * reads `GET /api/providers` and renders it flat, narrowed to the rows that
 * can take a first prompt.
 *
 * The read is deliberately *not* polled: the menu changes when the user edits
 * their launch configurations on the desktop, and the surfaces that show it
 * read it when they mount. A failed read keeps the error next to a retry
 * rather than pretending the menu is empty.
 *
 * The read is also **secondary**: it never reports an authorization failure
 * on its own. The surface's own read — the issue feed, the pull request feed
 * — owns the session-expiry signal, so a menu read can never sign the user
 * out of a screen the user has not even acted on yet. An expired session
 * fails that primary read too, and the app bounces to Connect from there.
 *
 * `enabled` lets a surface with no Spawn Menu at all skip the read entirely:
 * a surface with no split button has no reason to ask the desktop for one.
 */

import { useMemo, useState } from "react";
import { listProviders, type Provider } from "./api";
import { useAsyncEffect } from "../hooks/useAsyncEffect";
import { launchChoices, type LaunchChoice } from "./spawnMenu";

export function useSpawnMenu(enabled: boolean) {
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);

  useAsyncEffect(
    (signal) => {
      if (!enabled) return;
      setError(null);
      void listProviders()
        .then((rows) => {
          if (signal.aborted) return;
          // The menu is a list; anything else is a stale or mismatched
          // desktop, and rendering it as an empty menu would look like the
          // user has no agents at all.
          if (!Array.isArray(rows)) {
            throw new Error(
              "The desktop sent an unexpected launch menu. Update Buildmesh or refresh to retry.",
            );
          }
          setProviders(rows);
        })
        .catch((e) => {
          if (signal.aborted) return;
          setError((e as Error).message);
        });
    },
    [enabled, retry],
  );

  const choices = useMemo<LaunchChoice[] | null>(
    () => (enabled && providers ? launchChoices(providers) : null),
    [enabled, providers],
  );

  return {
    /** The menu rows, or `null` while the read is in flight. */
    choices,
    /** The raw backend rows — the mesh default label resolves against these. */
    providers: providers ?? [],
    error,
    retry: () => setRetry((value) => value + 1),
  };
}
