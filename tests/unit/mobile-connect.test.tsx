/**
 * Connect screen: token validation before the ?token= reload.
 *
 * Navigating straight to /?token=BAD lands on the embedded server's raw 401
 * page with no way back to the form, so Connect must probe the token via
 * /api/meshes first and show an inline error instead.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import Connect from "../../src/mobile/screens/Connect";

function mockFetchStatus(status: number) {
  const fn = vi.fn().mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    json: async () => [],
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

describe("Connect", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("shows an inline error on an invalid token and does not store it", async () => {
    const fetchMock = mockFetchStatus(401);
    const onConnected = vi.fn();
    render(<Connect onConnected={onConnected} />);

    await userEvent.type(screen.getByTestId("token-input"), "deadbeef");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-error").textContent).toMatch(
        /invalid token/i,
      );
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/meshes?token=deadbeef",
      expect.objectContaining({ credentials: "include" }),
    );
    expect(localStorage.getItem("buildmesh_token")).toBeNull();
    expect(onConnected).not.toHaveBeenCalled();
  });

  it("shows a reachability error when the desktop app is down", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("network")));
    render(<Connect onConnected={vi.fn()} />);

    await userEvent.type(screen.getByTestId("token-input"), "deadbeef");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(screen.getByTestId("connect-error").textContent).toMatch(
        /can't reach the desktop app/i,
      );
    });
  });

  it("stores a valid token before navigating", async () => {
    mockFetchStatus(200);
    render(<Connect onConnected={vi.fn()} />);

    await userEvent.type(screen.getByTestId("token-input"), "cafef00d");
    await userEvent.click(screen.getByTestId("connect-submit"));

    await waitFor(() => {
      expect(localStorage.getItem("buildmesh_token")).toBe("cafef00d");
    });
  });

  it("requires a token before submitting", async () => {
    const fetchMock = mockFetchStatus(200);
    render(<Connect onConnected={vi.fn()} />);

    await userEvent.click(screen.getByTestId("connect-submit"));

    expect(screen.getByTestId("connect-error").textContent).toMatch(
      /enter a token/i,
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("renders the notice explaining why the user landed here", () => {
    mockFetchStatus(200);
    render(<Connect onConnected={vi.fn()} notice="Session expired" />);
    expect(screen.getByTestId("connect-notice").textContent).toBe(
      "Session expired",
    );
  });
});
