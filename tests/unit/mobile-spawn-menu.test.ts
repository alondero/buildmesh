/**
 * The Spawn Menu on the phone (issue #2385).
 *
 * The phone renders the desktop's spawn menu on issues, pull requests and
 * meshes. Two contracts are pinned here:
 *
 *   1. **The wire shape.** A launch is one of three things — the mesh
 *      default, a plain Spawn Option, or a saved Launch Configuration — and
 *      each maps to a *literal* request body. The mesh default names
 *      neither `provider` nor `configuration_id`, which is what tells the
 *      backend to resolve its own default chain. A plain Spawn Option sends
 *      `provider` alone. A saved Launch Configuration sends the
 *      configuration's own `spawn_option_id` as `provider` beside the
 *      configuration's id as `configuration_id` — the same pair the Android
 *      client sends.
 *
 *   2. **The menu derivation.** The phone renders the backend's list as-is
 *      (ADR-0016); it only drops the rows that cannot take a first prompt,
 *      and it resolves a saved configuration row to the pair above.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  spawnFromIssue,
  spawnFromPull,
  type AgentNode,
  type GitHubIssue,
  type GitHubPullRequest,
  type Mesh,
  type Provider,
} from "../../src/mobile/api";
import {
  launchChoices,
  launchFields,
  meshDefaultLabel,
  meshDefaultProvider,
  MESH_DEFAULT_LABEL,
} from "../../src/mobile/spawnMenu";

const node: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: "fix-dark-mode",
  path: "/tmp/wt",
  branch: "main",
  provider: "anthropic",
  status: "running",
  cli_session_id: null,
  created_at: "2026-06-11T00:00:00Z",
};

const issue: GitHubIssue = {
  number: 42,
  title: "Add dark mode",
  body: "Please support dark themes.",
  url: "https://github.com/alondero/buildmesh/issues/42",
  state: "open",
  labels: ["enhancement"],
  blocked_by: [],
  author: "alondero",
};

const pull: GitHubPullRequest = {
  number: 12,
  title: "Fix the deadline",
  body: "",
  url: "https://github.com/alondero/buildmesh/pull/12",
  state: "open",
  draft: false,
  head_ref: "feat/deadline",
  head_repo_owner: "octo",
  head_repo_clone_url: "https://github.com/octo/buildmesh.git",
  head_sha: "abc123",
  author: "octo",
  mergeable: true,
  mergeable_state: "clean",
  checks_state: "success",
  review_decision: null,
};

/** A bare Spawn Option: the row launches itself. */
const plainOption = {
  id: "claude",
  label: "Claude Code",
  is_proxied: false,
  capabilities: { supports_prefill: true },
} as Provider;

/** A saved Launch Configuration: the row launches its own spawn option. */
const savedConfiguration = {
  ...plainOption,
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

describe("the phone's Spawn Menu (issue #2385)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(node), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  const body = () =>
    JSON.parse(fetchMock.mock.calls[0][1].body as string) as Record<
      string,
      unknown
    >;

  describe("the issue spawn route", () => {
    it("sends only the title for the mesh default", async () => {
      await spawnFromIssue(1, issue);

      expect(fetchMock.mock.calls[0][0]).toBe(
        "/api/meshes/1/issues/42/spawn",
      );
      expect(body()).toEqual({ title: "Add dark mode" });
    });

    it("sends the spawn option id for a plain Spawn Option", async () => {
      await spawnFromIssue(1, issue, {
        id: "claude",
        label: "Claude Code",
        provider: "claude",
      });

      expect(body()).toEqual({ title: "Add dark mode", provider: "claude" });
    });

    it("sends the configuration id beside its own spawn option for a saved Launch Configuration", async () => {
      await spawnFromIssue(1, issue, {
        id: "launch/saved-review",
        label: "Saved review",
        provider: "claude",
        configurationId: "launch/saved-review",
      });

      expect(body()).toEqual({
        title: "Add dark mode",
        provider: "claude",
        configuration_id: "launch/saved-review",
      });
    });
  });

  describe("the pull request spawn route", () => {
    it("sends the head ref and title for the mesh default", async () => {
      await spawnFromPull(1, pull);

      expect(fetchMock.mock.calls[0][0]).toBe("/api/meshes/1/pulls/12/spawn");
      expect(body()).toEqual({
        title: "Fix the deadline",
        head_ref: "feat/deadline",
        head_sha: "abc123",
        head_repo_owner: "octo",
        head_repo_clone_url: "https://github.com/octo/buildmesh.git",
      });
    });

    it("sends the spawn option id for a plain Spawn Option", async () => {
      await spawnFromPull(1, pull, {
        id: "claude",
        label: "Claude Code",
        provider: "claude",
      });

      expect(body()).toEqual({
        title: "Fix the deadline",
        head_ref: "feat/deadline",
        head_sha: "abc123",
        head_repo_owner: "octo",
        head_repo_clone_url: "https://github.com/octo/buildmesh.git",
        provider: "claude",
      });
    });

    it("sends the configuration id beside its own spawn option for a saved Launch Configuration", async () => {
      await spawnFromPull(1, pull, {
        id: "launch/saved-review",
        label: "Saved review",
        provider: "claude",
        configurationId: "launch/saved-review",
      });

      expect(body()).toEqual({
        title: "Fix the deadline",
        head_ref: "feat/deadline",
        head_sha: "abc123",
        head_repo_owner: "octo",
        head_repo_clone_url: "https://github.com/octo/buildmesh.git",
        provider: "claude",
        configuration_id: "launch/saved-review",
      });
    });

    it("omits the head repository for a same-repository pull request", async () => {
      // The backend keys its fork-spawn decision on whether the head owner is
      // `Some`, so an empty owner must be omitted rather than sent as "".
      await spawnFromPull(1, {
        ...pull,
        head_repo_owner: "",
        head_repo_clone_url: "",
        head_sha: "",
      });

      expect(body()).toEqual({
        title: "Fix the deadline",
        head_ref: "feat/deadline",
      });
    });
  });

  describe("the menu derivation", () => {
    it("keeps the backend's order and drops the rows that cannot take a prompt", () => {
      const choices = launchChoices([
        plainOption,
        { ...plainOption, id: "terminal", label: "Terminal", capabilities: { supports_prefill: false } },
        { ...plainOption, id: "agy", label: "AGY" },
      ] as Provider[]);

      expect(choices.map((choice) => choice.id)).toEqual(["claude", "agy"]);
    });

    it("drops an unavailable row", () => {
      const choices = launchChoices([
        { ...plainOption, unavailable_reason: "Harness is unavailable" },
      ] as Provider[]);

      expect(choices).toEqual([]);
    });

    it("resolves a saved Launch Configuration row to the pair the routes take", () => {
      const [choice] = launchChoices([savedConfiguration] as Provider[]);

      expect(choice).toEqual({
        id: "launch/saved-review",
        label: "Saved review",
        provider: "claude",
        configurationId: "launch/saved-review",
      });
      expect(launchFields(choice)).toEqual({
        provider: "claude",
        configuration_id: "launch/saved-review",
      });
    });

    it("sends neither field for the mesh default", () => {
      expect(launchFields(null)).toEqual({});
    });
  });

  describe("the mesh default label", () => {
    it("names the mesh's pinned default", () => {
      expect(
        meshDefaultLabel({ default_provider: "claude" } as Mesh, [
          plainOption,
        ] as Provider[]),
      ).toBe("Claude Code");
    });

    it("reads Mesh default when the mesh pins nothing the phone can see", () => {
      expect(
        meshDefaultLabel({ default_provider: null } as Mesh, [
          plainOption,
        ] as Provider[]),
      ).toBe(MESH_DEFAULT_LABEL);
    });
  });

  describe("the mesh default provider", () => {
    // The mark beside the split button's label is drawn from this row, so it
    // must be the *pinned* Spawn Option.
    it("resolves the mesh's pinned default Spawn Option", () => {
      expect(
        meshDefaultProvider({ default_provider: "claude" } as Mesh, [
          plainOption,
        ] as Provider[]),
      ).toBe(plainOption);
    });

    it("resolves no row when the mesh pins no default the phone can see", () => {
      // An app-wide default is invisible to the phone, so there is no row to
      // draw a mark from: the label alone is the honest answer.
      expect(
        meshDefaultProvider({ default_provider: null } as Mesh, [
          plainOption,
        ] as Provider[]),
      ).toBeNull();
      expect(
        meshDefaultProvider(null, [plainOption] as Provider[]),
      ).toBeNull();
    });

    it("ignores a saved Launch Configuration row that carries the pinned id", () => {
      // A saved configuration is a recipe, not a Spawn Option; the mark must
      // not come from one.
      expect(
        meshDefaultProvider({ default_provider: "launch/saved-review" } as Mesh, [
          savedConfiguration,
        ] as Provider[]),
      ).toBeNull();
    });
  });
});
