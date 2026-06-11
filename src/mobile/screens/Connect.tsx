import { useEffect, useState } from "react";
import { readStoredToken, rememberToken, validateToken } from "../api";

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

  // If the page was opened with `?token=` (initial QR code scan), persist it
  // and proceed immediately. The server has already set the bm_session cookie
  // on this exact request, so no extra round-trip is needed.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const urlToken = params.get("token");
    if (urlToken) {
      rememberToken(urlToken);
      onConnected();
    }
  }, [onConnected]);

  // Probe the token against the API first: navigating straight to ?token=
  // with a bad token would land on the server's raw 401 page with no way
  // back to this form. Only reload (to get the HttpOnly cookie set) once we
  // know the token is good.
  const connectWith = async (token: string) => {
    setBusy(true);
    setError(null);
    try {
      const ok = await validateToken(token);
      if (!ok) {
        setError("Invalid token — re-scan the QR code from the desktop app.");
        setBusy(false);
        return;
      }
      rememberToken(token);
      window.location.href =
        window.location.pathname + "?token=" + encodeURIComponent(token);
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
