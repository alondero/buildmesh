import { useEffect, useState } from "react";
import { readStoredToken, rememberToken } from "../api";

type Props = {
  onConnected: () => void;
};

export default function Connect({ onConnected }: Props) {
  const [tokenInput, setTokenInput] = useState("");
  const [error, setError] = useState<string | null>(null);
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

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!tokenInput.trim()) {
      setError("Enter a token");
      return;
    }
    rememberToken(tokenInput.trim());
    // The cookie will be set when the page reloads with ?token=, so reload
    // rather than just calling onConnected — that ensures the next /api call
    // succeeds even if cookies were the only thing we were missing.
    window.location.href =
      window.location.pathname +
      "?token=" +
      encodeURIComponent(tokenInput.trim());
  };

  const useStored = () => {
    if (!stored) return;
    // Same reasoning: reload via ?token= so the cookie gets set on this load.
    window.location.href =
      window.location.pathname + "?token=" + encodeURIComponent(stored);
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
      <h1 style={{ fontSize: 22, fontWeight: 600, color: "#fff", margin: 0 }}>
        Buildmesh Remote
      </h1>
      <p
        style={{
          color: "#888",
          fontSize: 13,
          textAlign: "center",
          maxWidth: 320,
          margin: 0,
        }}
      >
        Scan the QR code from your desktop app, or paste the token below.
      </p>

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
          style={{
            background: "#1a1a1a",
            border: "1px solid #333",
            borderRadius: 8,
            padding: "12px 14px",
            color: "#e0e0e0",
            fontSize: 14,
            outline: "none",
          }}
        />
        {error && (
          <span style={{ color: "#f44336", fontSize: 12 }}>{error}</span>
        )}
        <button
          type="submit"
          data-testid="connect-submit"
          style={{
            background: "#2196f3",
            border: "none",
            borderRadius: 8,
            padding: "12px 24px",
            color: "#fff",
            fontSize: 14,
            fontWeight: 500,
            cursor: "pointer",
          }}
        >
          Connect
        </button>
      </form>

      {stored && (
        <button
          onClick={useStored}
          data-testid="use-saved"
          style={{
            marginTop: 12,
            background: "transparent",
            border: "1px solid #333",
            borderRadius: 8,
            padding: "10px 20px",
            color: "#aaa",
            fontSize: 13,
            cursor: "pointer",
          }}
        >
          Use saved session
        </button>
      )}
    </main>
  );
}
