/**
 * The Spawn Menu surfaces on the phone (issue #2385).
 *
 * The desktop's spawn menu appears on the phone in three places — an issue
 * card, an open pull request card and a mesh card — and each one is a split
 * button: the main half runs the surface's default launch, the chevron opens
 * the Spawn Menu so the user can pick another one.
 *
 * This file pins the *surface* contract:
 *
 *   * the mesh card's split button opens Capture with that mesh and the
 *     chosen launch already selected;
 *   * Capture offers **Mesh default**, and picking it sends a launch that
 *     names neither `provider` nor `configuration_id`;
 *   * the pull request card's split button starts a launch with the mesh
 *     default, and the menu sends the picked launch's pair.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import NodeList from "../../src/mobile/screens/NodeList";
import PullRequestsScreen from "../../src/mobile/screens/PullRequests";
import type { AgentNode, Mesh, Provider } from "../../src/mobile/api";
import type { GitHubPullRequestFeed } from "../../src/types/generated/GitHubPullRequestFeed";

const mesh = {
  id: 1,
  name: "Buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
} as Mesh;

const node = {
  id: 7,
  mesh_id: 1,
  name: "fix-thing",
  path: "/tmp/wt",
  branch: "main",
  provider: "anthropic",
  status: "idle",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
} as AgentNode;

const provider = {
  id: "claude",
  label: "Claude Code",
  is_proxied: false,
  capabilities: { supports_prefill: true },
} as Provider;

const savedConfiguration = {
  ...provider,
  id: "launch/saved-review",
  label: "Saved review",
  configuration: {
    id: "launch/saved-review",
    name: "Saved review",
    spawn_option_id: "claude",
    model: "opus",
    effort: null,
    extra_args: null,
  },
} as Provider;

const pullFeed: GitHubPullRequestFeed = {
  items: [
    {
      number: 12,
      title: "Fix the deadline",
      body: "",
      url: "https://github.com/o/r/pull/12",
      state: "open",
      draft: false,
      head_ref: "feat/deadline",
      head_repo_owner: "octo",
      head_repo_clone_url: "https://github.com/octo/r.git",
      head_sha: "abc123",
      author: "octo",
      mergeable: true,
      mergeable_state: "clean",
      checks_state: "success",
      review_decision: null,
    },
  ],
  completeness: {
    returned: 1,
    pages_fetched: 1,
    complete: true,
    incomplete_reason: null,
    reported_total: null,
  },
};

const response = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status });

let writes: { path: string; body: Record<string, unknown> }[];

beforeEach(() => {
  localStorage.clear();
  writes = [];
  vi.stubGlobal(
    "WebSocket",
    class {
      close() {}
    },
  );
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, options?: RequestInit) => {
      if (options?.method === "POST") {
        writes.push({
          path: url,
          body: JSON.parse(options.body as string) as Record<string, unknown>,
        });
        if (url === "/api/nodes/create") {
          return response({ outcome: "active", node, operation_key: null });
        }
        return response(node);
      }
      if (url === "/api/meshes") return response([mesh]);
      if (url === "/api/nodes") return response([]);
      if (url === "/api/providers") {
        return response([provider, savedConfiguration]);
      }
      if (url.includes("/pulls")) return response(pullFeed);
      return response({});
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("the mesh card's spawn menu (issue #2385)", () => {
  it("opens Capture with the mesh and the mesh default already chosen", async () => {
    render(
      <NodeList
        onOpenNode={vi.fn()}
        onOpenAgentNodes={vi.fn()}
        onOpenIssues={vi.fn()}
        onOffline={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByTestId("mesh-spawn-1"));

    // Capture replaces the list, and it opens on the mesh's default launch.
    const agent = await screen.findByLabelText("Agent");
    expect((agent as HTMLSelectElement).value).toBe("");
    expect(screen.getByRole("option", { name: "Mesh default" })).toBeTruthy();
    expect((screen.getByLabelText("Work in") as HTMLSelectElement).value).toBe(
      "1",
    );
  });

  it("opens Capture with the launch picked from the menu", async () => {
    render(
      <NodeList
        onOpenNode={vi.fn()}
        onOpenAgentNodes={vi.fn()}
        onOpenIssues={vi.fn()}
        onOffline={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByTestId("mesh-spawn-1-menu"));
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText("Saved review");
    fireEvent.click(within(dialog).getByText("Saved review"));

    const agent = await screen.findByLabelText("Agent");
    expect((agent as HTMLSelectElement).value).toBe("launch/saved-review");
  });

  it("sends no provider at all when Capture runs with the mesh default", async () => {
    render(
      <NodeList
        onOpenNode={vi.fn()}
        onOpenAgentNodes={vi.fn()}
        onOpenIssues={vi.fn()}
        onOffline={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByTestId("mesh-spawn-1"));
    fireEvent.change(await screen.findByLabelText("Your idea"), {
      target: { value: "Persist this idea" },
    });
    fireEvent.click(screen.getByText("Start working on this"));

    await waitFor(() =>
      expect(
        writes.some((write) => write.path === "/api/nodes/create"),
      ).toBe(true),
    );
    const created = writes.find((write) => write.path === "/api/nodes/create");
    expect(created?.body).not.toHaveProperty("provider");
    expect(created?.body).not.toHaveProperty("configuration_id");
    expect(created?.body).toMatchObject({
      mesh_id: 1,
      prompt: "Persist this idea",
    });
  });
});

describe("the pull request card's spawn menu (issue #2385)", () => {
  it("starts the mesh default from the main half", async () => {
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByTestId("pr-spawn-12"));

    await waitFor(() =>
      expect(
        writes.some((write) =>
          write.path.endsWith("/api/meshes/1/pulls/12/spawn"),
        ),
      ).toBe(true),
    );
    const spawn = writes.find((write) =>
      write.path.endsWith("/api/meshes/1/pulls/12/spawn"),
    );
    expect(spawn?.body).toEqual({
      title: "Fix the deadline",
      head_ref: "feat/deadline",
      head_sha: "abc123",
      head_repo_owner: "octo",
      head_repo_clone_url: "https://github.com/octo/r.git",
    });
  });

  it("starts the launch picked from the menu", async () => {
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByTestId("pr-spawn-12-menu"));
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText("Saved review");
    fireEvent.click(within(dialog).getByText("Saved review"));

    await waitFor(() =>
      expect(
        writes.some((write) =>
          write.path.endsWith("/api/meshes/1/pulls/12/spawn"),
        ),
      ).toBe(true),
    );
    const spawn = writes.find((write) =>
      write.path.endsWith("/api/meshes/1/pulls/12/spawn"),
    );
    expect(spawn?.body).toMatchObject({
      provider: "claude",
      configuration_id: "launch/saved-review",
    });
  });
});
