import { useEffect, useRef, useState } from "react";
import { login, restoreSession, clearStoredToken } from "../api";

type Props = {
  onConnected: () => void;
  /// Optional one-line explanation of why the user landed here
  /// (e.g. "Session expired").
  notice?: string | null;
};

export default function Connect({ onConnected, notice }: Props) {
  const [tokenInput, setTokenInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const mounted = useRef(true);
  const inFlight = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  // Scrub the fragment before exchanging it; guard StrictMode's effect replay.
  const consumedTokenRef = useRef(false);
  useEffect(() => {
    const params = new URLSearchParams(window.location.hash.slice(1));
    const urlToken = params.get("pair");
    if (!urlToken) return;
    if (consumedTokenRef.current) return;
    consumedTokenRef.current = true;
    // Drop the token from the address bar regardless of outcome — it should
    // never linger in history.
    window.history.replaceState(
      null,
      "",
      window.location.pathname,
    );
    setTokenInput(urlToken);
    connectWith(urlToken);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- empty deps on purpose: the QR-token / paste-token dance is a one-shot on screen mount. Re-running on `connectWith` / `params` would re-fire the exchange on every parent render and waste the HttpOnly cookie.
  }, []);

  // Exchange the invitation for an HttpOnly session cookie via POST /api/pair.
  // An invalid invitation reports inline; an unreachable
  // app throws and we show the network hint.
  const connectWith = async (token: string) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const paired = await login(token);
      if (!mounted.current) return;
      if (!paired) {
        setError("Pairing code expired or already used. Get a new code from the desktop app.");
        setBusy(false);
        return;
      }
      clearStoredToken();
      onConnected();
    } catch {
      if (!mounted.current) return;
      setError(
        "Can't reach the desktop app. Is Buildmesh running and on the same network?",
      );
      setBusy(false);
    } finally {
      inFlight.current = false;
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const token = tokenInput.trim();
    if (!token) {
      setError("Enter a pairing code");
      return;
    }
    connectWith(token);
  };

  return (
    <main
      data-testid="connect-screen"
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
        gap: 12,
      }}
    >
      <div
        aria-hidden
        style={{
          width: 56,
          height: 56,
          borderRadius: 16,
          background: "linear-gradient(135deg, var(--accent), var(--accent-dim))",
          color: "var(--on-accent)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontSize: 26,
          marginBottom: 4,
        }}
      >
        ⬡
      </div>
      <h1 style={{ fontSize: 22, fontWeight: 600, color: "var(--text)", margin: 0 }}>
        Buildmesh Remote
      </h1>
      <p
        style={{
          color: "var(--text-dim)",
          fontSize: 13,
          textAlign: "center",
          maxWidth: 320,
          margin: 0,
          lineHeight: 1.5,
        }}
      >
        Scan the QR code from your desktop app, or paste a pairing code below.
        This browser stays paired until you revoke it or clear its site data.
      </p>

      {notice && (
        <p
          data-testid="connect-notice"
          style={{
            color: "var(--amber)",
            fontSize: 12,
            textAlign: "center",
            maxWidth: 320,
            margin: 0,
          }}
        >
          {notice}
        </p>
      )}

      <form
        onSubmit={handleSubmit}
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 8,
          width: "100%",
          maxWidth: 320,
          marginTop: 16,
        }}
      >
        <input
          type="password"
          inputMode="text"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          placeholder="Paste pairing code"
          autoComplete="off"
          value={tokenInput}
          onChange={(e) => {
            setTokenInput(e.target.value);
            setError(null);
          }}
          data-testid="token-input"
          className="field"
        />
        {error && (
          <span
            data-testid="connect-error"
            style={{ color: "var(--red)", fontSize: 12 }}
          >
            {error}
          </span>
        )}
        <button
          type="submit"
          disabled={busy}
          data-testid="connect-submit"
          className="btn-primary"
          style={{ padding: "12px 24px" }}
        >
          {busy ? "Connecting…" : "Connect"}
        </button>
      </form>

        <button
          onClick={async () => {
            if (inFlight.current) return;
            inFlight.current = true;
            setBusy(true);
            setError(null);
            try {
              const restored = await restoreSession();
              if (!mounted.current) return;
              if (restored) onConnected();
              else setError("This browser is not paired. Scan a new code from the desktop app.");
            } catch {
              if (mounted.current) setError("Can't reach the desktop app. Check your connection and try again.");
            } finally {
              inFlight.current = false;
              if (mounted.current) setBusy(false);
            }
          }}
          disabled={busy}
          data-testid="use-saved"
          className="btn-ghost"
          style={{ marginTop: 12 }}
        >
          Reconnect paired phone
        </button>
    </main>
  );
}
