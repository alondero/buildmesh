import { useCallback, useEffect, useRef, useState } from 'react';
import {
  addHarnessLogin,
  cancelHarnessLoginSignIn,
  listHarnessLogins,
  removeHarnessLogin,
  renameHarnessLogin,
  startHarnessLoginSignIn,
  type HarnessLoginStatus,
} from '../../lib/tauri/provider';
import { formatError } from '../../lib/errorUtils';
import { publishHarnessLogins } from '../../hooks/useHarnessLogins';
import { SettingsSection } from './SettingsRow';
import { SafeLink } from '../shared/SafeLink';

/** The login commands, injectable so tests drive the section without IPC. */
export interface CodexLoginsApi {
  list: () => Promise<HarnessLoginStatus[]>;
  add: (label: string) => Promise<unknown>;
  rename: (id: string, label: string) => Promise<unknown>;
  remove: (id: string) => Promise<void>;
  startSignIn: (id: string) => Promise<unknown>;
  cancelSignIn: (id: string) => Promise<void>;
}

const defaultApi: CodexLoginsApi = {
  list: listHarnessLogins,
  add: addHarnessLogin,
  rename: renameHarnessLogin,
  remove: removeHarnessLogin,
  startSignIn: startHarnessLoginSignIn,
  cancelSignIn: cancelHarnessLoginSignIn,
};

/** How often the list is re-read while `codex login` waits for the browser. */
export const SIGN_IN_POLL_MS = 2000;

const secondaryButton = 'px-2.5 py-1 border border-border-strong text-text-secondary text-sm rounded-md hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-50';
const primaryButton = 'px-2.5 py-1 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-sm rounded-md hover:bg-bg-card-hover disabled:opacity-50';
const inputClass = 'min-w-0 flex-1 rounded-md border border-border-default bg-bg-card px-2 py-1 text-sm text-text-primary';

function statusLine(login: HarnessLoginStatus): string {
  if (!login.signed_in) return 'Not signed in';
  const who = login.email ?? 'Signed in';
  return login.plan ? `${who} · ${login.plan}` : who;
}

/**
 * Settings › Launch Configurations › Codex logins: add, rename, remove and sign in to
 * additional ChatGPT accounts for Codex. Each login has its own Codex folder;
 * with more than one, the spawn menu asks which to launch with and Usage
 * shows a meter per login.
 */
export function CodexLoginsSection({ api = defaultApi }: { api?: CodexLoginsApi }) {
  const [logins, setLogins] = useState<HarnessLoginStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [newLabel, setNewLabel] = useState('');
  const [renaming, setRenaming] = useState<{ id: string; label: string } | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const mounted = useRef(true);
  // Set on every mount: StrictMode runs the cleanup once and mounts again.
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const reply: unknown = await api.list();
      const list = Array.isArray(reply) ? reply as HarnessLoginStatus[] : [];
      if (!mounted.current) return;
      setLogins(list);
      // The spawn menu reads the same list; hand it the fresh sign-in state.
      publishHarnessLogins(list);
    } catch (e) {
      if (mounted.current) setError(formatError(e));
    }
  }, [api]);

  useEffect(() => { void refresh(); }, [refresh]);

  const signingIn = logins?.some((login) => login.sign_in?.running) ?? false;
  useEffect(() => {
    if (!signingIn) return;
    const timer = window.setInterval(() => { void refresh(); }, SIGN_IN_POLL_MS);
    return () => window.clearInterval(timer);
  }, [signingIn, refresh]);

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      await refresh();
    } catch (e) {
      if (mounted.current) setError(formatError(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  const copy = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url);
      setCopied(url);
      window.setTimeout(() => { if (mounted.current) setCopied(null); }, 2000);
    } catch (e) {
      setError(formatError(e));
    }
  };

  return (
    <SettingsSection
      title="Codex logins"
      testId="codex-logins"
      description={
        <>
          Sign Codex in to more than one ChatGPT account. Each login keeps its own
          Codex folder (sign-in, settings and sessions). With more than one login,
          the spawn menu asks which login to launch with and the Usage tab shows a
          meter per login. Logins apply to native Windows launches; WSL launches
          keep their own default login.
        </>
      }
    >
      <p className="mb-3 text-sm text-text-muted">
        Signing in opens your browser on whichever ChatGPT account it is already signed in to.
        Switch accounts there first, or open the sign-in link in a private window.
      </p>
      {logins === null && !error && <p className="text-sm text-text-muted">Loading logins…</p>}
      {logins && (
        <ul className="divide-y divide-border-subtle rounded-md border border-border-subtle" aria-label="Codex logins">
          {logins.map((login) => {
            const signIn = login.sign_in;
            const isRenaming = renaming?.id === login.id;
            return (
              <li key={login.id} className="px-3 py-2.5" data-login-id={login.id}>
                <div className="flex items-start gap-3">
                  <div className="min-w-0 flex-1">
                    {isRenaming ? (
                      <form className="flex gap-2" onSubmit={(e) => {
                        e.preventDefault();
                        const label = renaming.label;
                        void run(async () => { await api.rename(login.id, label); setRenaming(null); });
                      }}>
                        <input aria-label={`New name for ${login.label}`} autoFocus className={inputClass} maxLength={40}
                          value={renaming.label} onChange={(e) => setRenaming({ id: login.id, label: e.target.value })} />
                        <button type="submit" className={primaryButton} disabled={busy || !renaming.label.trim()}>Save</button>
                        <button type="button" className={secondaryButton} onClick={() => setRenaming(null)}>Cancel</button>
                      </form>
                    ) : (
                      <p className="text-base font-medium text-text-primary">{login.label}</p>
                    )}
                    <p className="text-sm text-text-secondary">{statusLine(login)}</p>
                    <p className="truncate text-2xs text-text-muted" title={login.home}>{login.home}</p>
                    {login.same_account_as && (
                      <p role="note" className="text-sm text-status-warning">
                        Signed in to the same ChatGPT account as {login.same_account_as}. Sign in again with the other account.
                      </p>
                    )}
                  </div>
                  {!isRenaming && (
                    <div className="flex shrink-0 gap-2">
                      {signIn?.running ? (
                        <button type="button" className={secondaryButton} disabled={busy}
                          onClick={() => void run(() => api.cancelSignIn(login.id))}>Cancel sign-in</button>
                      ) : (
                        <button type="button" className={primaryButton} disabled={busy || signingIn}
                          onClick={() => void run(() => api.startSignIn(login.id))}>
                          {login.signed_in ? 'Sign in again' : 'Sign in'}
                        </button>
                      )}
                      {!login.is_default && <>
                        <button type="button" className={secondaryButton} disabled={busy}
                          onClick={() => setRenaming({ id: login.id, label: login.label })}>Rename</button>
                        <button type="button" className={secondaryButton} disabled={busy}
                          onClick={() => setConfirmRemove(login.id)}>Remove</button>
                      </>}
                    </div>
                  )}
                </div>
                {signIn?.running && (
                  <div className="mt-2 rounded-md bg-bg-card px-2.5 py-2 text-sm text-text-secondary" role="status">
                    {signIn.url ? (
                      <>
                        Waiting for the browser sign-in. If the wrong account opened, open this link in a private window:
                        <div className="mt-1 flex items-center gap-2">
                          <SafeLink url={signIn.url} className="min-w-0 truncate text-accent-cyan hover:underline">{signIn.url}</SafeLink>
                          <button type="button" className={secondaryButton} onClick={() => void copy(signIn.url!)}>
                            {copied === signIn.url ? 'Copied' : 'Copy link'}
                          </button>
                        </div>
                      </>
                    ) : 'Starting codex login…'}
                  </div>
                )}
                {signIn && !signIn.running && signIn.error && (
                  <p role="alert" className="mt-2 text-sm text-status-error">{signIn.error}</p>
                )}
                {confirmRemove === login.id && (
                  <div className="mt-2 rounded-md border border-border-subtle px-2.5 py-2 text-sm text-text-secondary" role="group" aria-label={`Remove ${login.label}`}>
                    Remove {login.label}? Launch Configurations pinned to it fall back to the menu pick. Its folder stays at {login.home}; adding a login with the same name later reuses it.
                    <div className="mt-2 flex gap-2">
                      <button type="button" className={primaryButton} disabled={busy}
                        onClick={() => void run(async () => { await api.remove(login.id); setConfirmRemove(null); })}>Remove login</button>
                      <button type="button" className={secondaryButton} onClick={() => setConfirmRemove(null)}>Keep</button>
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <form className="mt-3 flex gap-2" onSubmit={(e) => {
        e.preventDefault();
        const label = newLabel;
        void run(async () => { await api.add(label); setNewLabel(''); });
      }}>
        <input aria-label="New login name" placeholder="Login name, e.g. Work" className={inputClass} maxLength={40}
          value={newLabel} onChange={(e) => setNewLabel(e.target.value)} />
        <button type="submit" className={secondaryButton} disabled={busy || !newLabel.trim()}>Add login</button>
      </form>
      {error && <p role="alert" className="mt-2 text-sm text-status-error">{error}</p>}
    </SettingsSection>
  );
}
