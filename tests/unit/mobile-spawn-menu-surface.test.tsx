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
 *
 * It also pins the side effects the Spawn Menu must not have: a mesh card's
 * preset may not overwrite the user's stored destination, a launch in flight
 * owns the sheet, and going back closes the menu before the screen.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Profiler } from "react";
import NodeList from "../../src/mobile/screens/NodeList";
import PullRequestsScreen from "../../src/mobile/screens/PullRequests";
import CaptureIdea from "../../src/mobile/screens/CaptureIdea";
import type { AgentNode, Mesh, Provider } from "../../src/mobile/api";
import type { GitHubPullRequestFeed } from "../../src/types/generated/GitHubPullRequestFeed";

/** The key CaptureIdea stores the chosen destination under. */
const DESTINATION_KEY = "buildmesh_mobile_idea_destination";

const mesh = {
  id: 1,
  name: "Buildmesh",
  path: "/tmp/repo",
  created_at: "2026-06-11T00:00:00Z",
  scratchpad: "",
  sandbox: false,
  // The mesh pins its own default Spawn Option, so the split buttons carry
  // that provider's brand mark beside their label.
  default_provider: "claude",
} as Mesh;

/** A second mesh, so a stored destination can differ from the preset one. */
const otherMesh = {
  ...mesh,
  id: 2,
  name: "Website",
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
  harness_id: "claude",
  provider_id: null,
  icon: "A",
  capabilities: { supports_prefill: true, accepts_initial_prompt: true },
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
/** Holds a pull request spawn open, so a test can watch the in-flight state. */
let spawnPending: Promise<void> | null = null;

beforeEach(() => {
  localStorage.clear();
  writes = [];
  spawnPending = null;
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
        if (spawnPending) await spawnPending;
        return response(node);
      }
      if (url === "/api/meshes") return response([mesh, otherMesh]);
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

  it("draws the mesh default's brand mark beside the split button's label", async () => {
    render(
      <NodeList
        onOpenNode={vi.fn()}
        onOpenAgentNodes={vi.fn()}
        onOpenIssues={vi.fn()}
        onOffline={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    const start = await screen.findByTestId("mesh-spawn-1");
    // The mark is the pinned default's registered brand, not a neutral dot.
    expect(
      start.querySelector(".spawn-split-brand")?.tagName.toLowerCase(),
    ).toBe("svg");
    expect(start.querySelector(".spawn-split-label")?.textContent).toBe(
      "New agent",
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

describe("the Spawn Menu's side effects (issue #2385)", () => {
  /** Renders the mesh list, where the mesh card's split button lives. */
  function renderNodeList() {
    return render(
      <NodeList
        onOpenNode={vi.fn()}
        onOpenAgentNodes={vi.fn()}
        onOpenIssues={vi.fn()}
        onOffline={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );
  }

  /**
   * Opens Capture through the mesh card's Spawn Menu, with "Saved review"
   * picked — the mesh card's own choice of mesh and launch.
   */
  async function openCaptureWithPreset() {
    renderNodeList();
    fireEvent.click(await screen.findByTestId("mesh-spawn-1-menu"));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(await within(dialog).findByText("Saved review"));
    await screen.findByLabelText("Work in");
  }

  it("opens Capture on the preset without a second render", () => {
    localStorage.setItem(
      DESTINATION_KEY,
      JSON.stringify({ meshId: "2", providerId: "claude" }),
    );
    const commits: string[] = [];

    render(
      <Profiler
        id="capture"
        onRender={(_id, phase) => {
          commits.push(phase);
        }}
      >
        <CaptureIdea
          meshes={[mesh, otherMesh]}
          providers={[provider, savedConfiguration]}
          preset={{ meshId: 1, launchId: "launch/saved-review" }}
          onStarted={vi.fn()}
          onAuthFailed={vi.fn()}
        />
      </Profiler>,
    );

    // The preset is in the very first render: the stored destination never
    // flashes on screen, and the preset costs no second render.
    expect((screen.getByLabelText("Work in") as HTMLSelectElement).value).toBe(
      "1",
    );
    expect((screen.getByLabelText("Agent") as HTMLSelectElement).value).toBe(
      "launch/saved-review",
    );
    expect(commits).toEqual(["mount"]);
  });

  it("keeps the stored destination when the mesh card preselects a launch", async () => {
    localStorage.setItem(
      DESTINATION_KEY,
      JSON.stringify({ meshId: "2", providerId: "claude" }),
    );

    await openCaptureWithPreset();

    // Capture opens on the preset the mesh card chose…
    expect((screen.getByLabelText("Work in") as HTMLSelectElement).value).toBe("1");
    expect((screen.getByLabelText("Agent") as HTMLSelectElement).value).toBe(
      "launch/saved-review",
    );
    // …but the user's own stored destination is the user's, and survives it.
    expect(JSON.parse(localStorage.getItem(DESTINATION_KEY) ?? "{}")).toEqual({
      meshId: "2",
      providerId: "claude",
    });
  });

  it("stores the destination once the user picks their own", async () => {
    await openCaptureWithPreset();

    fireEvent.change(screen.getByLabelText("Work in"), {
      target: { value: "2" },
    });

    await waitFor(() =>
      expect(
        JSON.parse(localStorage.getItem(DESTINATION_KEY) ?? "{}"),
      ).toEqual({
        meshId: "2",
        providerId: "launch/saved-review",
      }),
    );
  });

  it("stores the agent the user picks after a preset", async () => {
    await openCaptureWithPreset();

    fireEvent.change(screen.getByLabelText("Agent"), {
      target: { value: "claude" },
    });

    await waitFor(() =>
      expect(
        JSON.parse(localStorage.getItem(DESTINATION_KEY) ?? "{}"),
      ).toEqual({
        meshId: "1",
        providerId: "claude",
      }),
    );
  });

  it("starts one agent when the launch is tapped twice", async () => {
    let release!: () => void;
    spawnPending = new Promise<void>((resolve) => {
      release = resolve;
    });
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    const start = await screen.findByTestId("pr-spawn-12");
    // Both taps land in the same tick, so the button is still live for the
    // second one: only the synchronous guard stops a second agent.
    act(() => {
      fireEvent.click(start);
      fireEvent.click(start);
    });

    await waitFor(() =>
      expect(
        writes.filter((write) =>
          write.path.endsWith("/api/meshes/1/pulls/12/spawn"),
        ),
      ).toHaveLength(1),
    );

    release();
    await waitFor(() =>
      expect(screen.getByText("Started an agent on PR #12.")).toBeTruthy(),
    );
  });

  it("locks the menu rows while a launch is in flight", async () => {
    let release!: () => void;
    spawnPending = new Promise<void>((resolve) => {
      release = resolve;
    });
    const onBack = vi.fn();
    const backHandlerRef: { current: (() => boolean) | null } = {
      current: null,
    };
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={onBack}
        onAuthFailed={vi.fn()}
        backHandlerRef={backHandlerRef}
      />,
    );

    fireEvent.click(await screen.findByTestId("pr-spawn-12-menu"));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(await within(dialog).findByText("Saved review"));

    // The launch owns the sheet: its rows are locked…
    await waitFor(() =>
      expect(
        screen.getByTestId("pr-launch-menu-launch/saved-review"),
      ).toHaveProperty("disabled", true),
    );
    // …the backdrop does not dismiss it…
    fireEvent.click(screen.getByTestId("pr-launch-menu-backdrop"));
    expect(screen.getByRole("dialog")).toBeTruthy();
    // …and back is consumed without closing it, so the screen stays too.
    fireEvent.click(screen.getByLabelText("Back"));
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(onBack).not.toHaveBeenCalled();
    await act(async () => {
      expect(backHandlerRef.current?.()).toBe(true);
    });
    expect(screen.getByRole("dialog")).toBeTruthy();

    release();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("dismisses the Spawn Menu before the screen goes back", async () => {
    const onBack = vi.fn();
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={onBack}
        onAuthFailed={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByTestId("pr-spawn-12-menu"));
    await screen.findByRole("dialog");

    fireEvent.click(screen.getByLabelText("Back"));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(onBack).not.toHaveBeenCalled();

    // The next back leaves the screen.
    fireEvent.click(screen.getByLabelText("Back"));
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it("hands the app's back gesture to the open menu", async () => {
    const onBack = vi.fn();
    const backHandlerRef: { current: (() => boolean) | null } = {
      current: null,
    };
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={onBack}
        onAuthFailed={vi.fn()}
        backHandlerRef={backHandlerRef}
      />,
    );

    fireEvent.click(await screen.findByTestId("pr-spawn-12-menu"));
    await screen.findByRole("dialog");

    let handled = false;
    await act(async () => {
      handled = backHandlerRef.current?.() ?? false;
    });
    expect(handled).toBe(true);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(onBack).not.toHaveBeenCalled();

    // With no menu open the gesture belongs to the screen again.
    await act(async () => {
      handled = backHandlerRef.current?.() ?? false;
    });
    expect(handled).toBe(false);
  });
});

describe("the pull request card's spawn menu (issue #2385)", () => {
  it("draws the mesh default's brand mark beside the split button's label", async () => {
    render(
      <PullRequestsScreen
        mesh={mesh}
        onBack={vi.fn()}
        onAuthFailed={vi.fn()}
      />,
    );

    const start = await screen.findByTestId("pr-spawn-12");
    // The mark is the pinned default's registered brand, not a neutral dot.
    expect(
      start.querySelector(".spawn-split-brand")?.tagName.toLowerCase(),
    ).toBe("svg");
    expect(start.querySelector(".spawn-split-label")?.textContent).toBe(
      "Claude Code",
    );
  });

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
