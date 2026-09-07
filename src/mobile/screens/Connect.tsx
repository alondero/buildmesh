import { useEffect, useRef, useState } from "react";
import { login, readStoredToken, rememberToken } from "../api";

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
  const stored = readStoredToken();

  // If the page was opened with `?token=` (initial QR code scan), read the
  // token from the URL via JS, exchange it for the bm_session cookie, then
  // strip it from the URL (issue #500 — the token is never sent to the server
  // as a query param it validates). The shell loaded publicly to run this code.
  //
  // The `consumedTokenRef` guard (issue #1260) makes the side-effect
  // idempotent across React StrictMode's simulated remount in development:
  // refs persist across the simulated unmount/remount cycle, while the
  // replaceState below ALSO strips the token from window.location.search,
  // so either barrier is sufficient in a well-behaved environment — both
  // together make a single POST /api/session per page load guaranteed even
  // if the URL ever survives between the two effect runs (older React
  // versions, browser quirks, future changes).
  const consumedTokenRef = useRef(false);
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const urlToken = params.get("token");
    if (!urlToken) return;
    if (consumedTokenRef.current) return;
    consumedTokenRef.current = true;
    // Drop the token from the address bar regardless of outcome — it should
    // never linger in history.
    params.delete("token");
    const rest = params.toString();
    window.history.replaceState(
      null,
      "",
      window.location.pathname + (rest ? "?" + rest : ""),
    );
    connectWith(urlToken);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- connectWith captured on mount; the screen re-mounts when the user re-opens with a new token.
  }, []);

  // Exchange the token for the HttpOnly session cookie via POST /api/session.
  // A bad token reports inline (no dead-end on a raw 401 page); an unreachable
  // app throws and we show the network hint.
  const connectWith = async (token: string) => {
    setBusy(true);
    setError(null);
    try {
      const deviceToken = await login(token);
      if (!deviceToken) {
        setError("Invalid token — re-scan the QR code from the desktop app.");
        setBusy(false);
        return;
      }
      // Persist the per-device token the server returned (issue #502), not the
      // root token we may have pasted — this device is now revocable on its own.
      rememberToken(deviceToken);
      onConnected();
    } catch {
      setError(
        "Can't reach the desktop app. Is Buildmesh running and on the same network?",
      );
      setBusy(false);
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const token = tokenInput.trim();
    if (!token) {
      setError("Enter a token");
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
          background: "linear-gradient(135deg, #2196f3, #0d47a1)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontSize: 26,
          marginBottom: 4,
        }}
      >
        ⬡
      </div>
      <h1 style={{ fontSize: 22, fontWeight: 600, color: "#fff", margin: 0 }}>
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
        Scan the QR code from your desktop app, or paste the token below.
      </p>

      {notice && (
        <p
          data-testid="connect-notice"
          style={{
            color: "#ffb74d",
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
          type="text"
          inputMode="text"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          placeholder="paste token"
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

      {stored && (
        <button
          onClick={() => connectWith(stored)}
          disabled={busy}
          data-testid="use-saved"
          className="btn-ghost"
          style={{ marginTop: 12 }}
        >
          Use saved session
        </button>
      )}
    </main>
  );
}
