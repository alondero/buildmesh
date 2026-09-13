import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Connect from "../../src/mobile/screens/Connect";
import { restoreSession } from "../../src/mobile/api";

function response(status: number) { return new Response(null, { status }); }

describe("durable mobile pairing", () => {
  beforeEach(() => {
    localStorage.clear();
    window.history.replaceState(null, "", "/");
  });
  afterEach(() => vi.unstubAllGlobals());

  it("exchanges a fragment once under StrictMode and never persists a secret in JS storage", async () => {
    window.history.replaceState(null, "", "/#pair=single-use-code");
    const fetchMock = vi.fn().mockImplementation(() => {
      expect(window.location.hash).toBe("");
      return Promise.resolve(response(204));
    });
    vi.stubGlobal("fetch", fetchMock);
    const onConnected = vi.fn();
    render(<React.StrictMode><Connect onConnected={onConnected} /></React.StrictMode>);
    await waitFor(() => expect(onConnected).toHaveBeenCalledTimes(1));
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith("/api/pair", {
      method: "POST", headers: { Authorization: "Bearer single-use-code" }, credentials: "include",
    });
    expect(localStorage.length).toBe(0);
    expect(window.location.search).toBe("");
  });

  it("pairs by manual paste and clears a legacy saved credential", async () => {
    localStorage.setItem("buildmesh_token", "old-secret");
    const fetchMock = vi.fn().mockResolvedValue(response(204));
    vi.stubGlobal("fetch", fetchMock);
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);
    await userEvent.type(screen.getByTestId("token-input"), "manual-code");
    await userEvent.click(screen.getByTestId("connect-submit"));
    await waitFor(() => expect(onConnected).toHaveBeenCalledOnce());
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
    expect(fetchMock.mock.calls[0][1].headers.Authorization).toBe("Bearer manual-code");
  });

  it("explains expired or consumed invitations without storing them", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response(401)));
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);
    await userEvent.type(screen.getByTestId("token-input"), "used-code");
    await userEvent.click(screen.getByTestId("connect-submit"));
    expect(await screen.findByText(/code expired or already used/i)).toBeTruthy();
    expect(onConnected).not.toHaveBeenCalled();
    expect(localStorage.length).toBe(0);
  });

  it("keeps manual input retryable when offline", async () => {
    const fetchMock = vi.fn().mockRejectedValueOnce(new TypeError("offline")).mockResolvedValue(response(204));
    vi.stubGlobal("fetch", fetchMock);
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);
    await userEvent.type(screen.getByTestId("token-input"), "retry-code");
    await userEvent.click(screen.getByTestId("connect-submit"));
    expect(await screen.findByText(/can't reach the desktop app/i)).toBeTruthy();
    await userEvent.click(screen.getByTestId("connect-submit"));
    await waitFor(() => expect(onConnected).toHaveBeenCalledOnce());
  });

  it("requires a code before submitting", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    render(<Connect onConnected={vi.fn()} />);
    await userEvent.click(screen.getByTestId("connect-submit"));
    expect(screen.getByText("Enter a pairing code")).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("restores a paired browser with its cookie alone", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(204));
    vi.stubGlobal("fetch", fetchMock);
    expect(await restoreSession()).toBe(true);
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith("/api/session", {
      method: "POST", credentials: "include", headers: undefined,
    });
  });

  it("migrates an existing device credential once and removes localStorage", async () => {
    localStorage.setItem("buildmesh_token", "legacy-device");
    const fetchMock = vi.fn().mockResolvedValueOnce(response(401)).mockResolvedValueOnce(response(204));
    vi.stubGlobal("fetch", fetchMock);
    expect(await restoreSession()).toBe(true);
    expect(fetchMock.mock.calls[1]).toEqual(["/api/session", {
      method: "POST", credentials: "include", headers: { Authorization: "Bearer legacy-device" },
    }]);
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
  });

  it("removes a revoked legacy credential, but preserves it across network failure", async () => {
    localStorage.setItem("buildmesh_token", "legacy-device");
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    await expect(restoreSession()).rejects.toThrow();
    expect(localStorage.getItem("buildmesh_token")).toBe("legacy-device");
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response(401)));
    expect(await restoreSession()).toBe(false);
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
  });

  it("does not exchange legacy root-token QR URLs", () => {
    window.history.replaceState(null, "", "/?token=old-root");
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    render(<Connect onConnected={vi.fn()} />);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
