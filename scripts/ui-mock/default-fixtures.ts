import type { Mesh } from '../../src/types/generated/Mesh';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { CircuitWithRuns } from '../../src/types/generated/CircuitWithRuns';
import type { CircuitQueueEntry } from '../../src/types/generated/CircuitQueueEntry';
import type { CircuitProbeSnapshot } from '../../src/types/generated/CircuitProbeSnapshot';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import type { CircuitAgentOwnership } from '../../src/types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../../src/types/generated/SemanticTurnPayload';
import type { GitBranchStatus } from '../../src/types/generated/GitBranchStatus';
import type { GitSummary } from '../../src/types/generated/GitSummary';
import type { AppPreferences } from '../../src/types/generated/AppPreferences';
import type { ProviderAccount } from '../../src/types/generated/ProviderAccount';
import type { NetworkStatus } from '../../src/types/generated/NetworkStatus';
import type { ProbeSpawnPromptDefaults } from '../../src/types/generated/ProbeSpawnPromptDefaults';
import type { PreferencesHealth } from '../../src/types/generated/PreferencesHealth';
import type { ProviderPairing } from '../../src/types/generated/ProviderPairing';
import type { PairingVerification } from '../../src/types/generated/PairingVerification';
import type { CoordinatorStatus } from '../../src/types/generated/CoordinatorStatus';
import type { DeviceSession } from '../../src/types/generated/DeviceSession';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import type { LaunchTarget } from '../../src/types/generated/LaunchTarget';
import type { OpenCodeConsoleStatus } from '../../src/types/generated/OpenCodeConsoleStatus';
import type { OpenPr } from '../../src/types/generated/OpenPr';
import type { GitStatus } from '../../src/types/generated/GitStatus';

/** Fresh wire responses for each browser, checked by the root tsc gate. */
export function buildDefaultFixtures() {
  const meshDefaults = {
    layout: 'grid', position: 0, created_at: '1970-01-01T00:00:00Z',
    build_command: null, run_command: null, model: null, effort: null,
    use_worktree: true, worktree_mode: 'perNode', default_provider: null,
    base_ref: 'origin/main', scratchpad: '', sandbox: false,
    pre_spawn_pool_size: 1, color: null, root_build_command: null, root_run_command: null,
    circuit_run_capacity: 2, worktree_directory: null,
  };

  const nodeDefaults = {
    env: 'windows' as const, provider: 'anthropic', cli_session_id: null,
    worktree_name: null, use_worktree: true, source_issue: null,
    source_pr: null, head_repo_owner: null, head_repo_clone_url: null,
    source_pr_pinned_sha: null, is_pinned: false, signal_health: null, worktree_path: null, created_at: '1970-01-01T00:00:00Z',
  };

  const meshes = [
    { ...meshDefaults, id: 1, name: 'buildmesh', path: '/home/user/buildmesh', position: 0, color: '#6366f1' },
    { ...meshDefaults, id: 2, name: 'playground', path: '/home/user/playground', position: 1, color: '#10b981' },
  ] satisfies Mesh[];

  const agentNodes = [
    { ...nodeDefaults, id: 1, mesh_id: 1, name: 'fix-terminal-blank', path: '/home/user/buildmesh', branch: 'origin/main', status: 'running', position: 0 },
    { ...nodeDefaults, id: 2, mesh_id: 1, name: 'diff-panel-refactor', path: '/home/user/buildmesh', branch: 'origin/main', status: 'awaiting_input', position: 1 },
    { ...nodeDefaults, id: 3, mesh_id: 2, name: 'scratch', path: '/home/user/playground', branch: 'origin/main', status: 'idle', position: 0 },
  ] satisfies AgentNode[];

  const circuitsWithRuns = [
    {
      circuit: {
        is_preset: false,
        id: 101,
        mesh_id: 1,
        name: 'Issue review loop',
        description: 'Review labelled issues and report the result.',
        enabled: true,
        graph_json: '{"nodes":[],"edges":[]}',
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      },
      runs: [
        {
          run: {
            source_agent_node_id: null, attention_dismissed_at: null,
            id: 1001,
            circuit_id: 101,
            mesh_id: 1,
            trigger_identity: 'issue:1468:buildmesh:run',
            state: 'failed',
            context_json: '{}',
            created_at: '2026-01-01T00:00:00Z',
            updated_at: '2026-01-01T00:02:00Z',
          },
          steps: [
            {
              id: 1002,
              run_id: 1001,
              node_id: 'reviewer',
              agent_node_id: 2,
              status: 'failed',
              attempt: 1,
              outcome: 'failed',
              error_message: 'The review command exited before producing a result.',
              started_at: '2026-01-01T00:01:00Z',
              completed_at: '2026-01-01T00:02:00Z',
            },
          ],
        },
      ],
    },
  ] satisfies CircuitWithRuns[];

  const circuitQueue = [
    {
      run: {
        source_agent_node_id: null, attention_dismissed_at: null,
        id: 1002,
        circuit_id: 101,
        mesh_id: 1,
        trigger_identity: 'issue:1501:buildmesh:run',
        state: 'pending',
        context_json: '{}',
        created_at: '2026-01-01T00:03:00Z',
        updated_at: '2026-01-01T00:03:00Z',
      },
      circuit_name: 'Issue review loop',
      queue_rank: 1,
    },
    {
      run: {
        source_agent_node_id: null, attention_dismissed_at: null,
        id: 1003,
        circuit_id: 101,
        mesh_id: 1,
        trigger_identity: 'issue:1502:buildmesh:run',
        state: 'pending',
        context_json: '{}',
        created_at: '2026-01-01T00:04:00Z',
        updated_at: '2026-01-01T00:04:00Z',
      },
      circuit_name: 'Issue review loop',
      queue_rank: 2,
    },
  ] satisfies CircuitQueueEntry[];

  const providers = [{
    id: 'anthropic', label: 'Claude', color: '#d97757', icon: 'C',
    resumable: true, harness_id: 'anthropic', provider_id: null,
    is_proxied: false, group_key: 'anthropic',
    capabilities: {
      harness_id: 'anthropic', background_inference: null,
      supports_resume: true, auto_resume_on_startup: true,
      requires_attention_hook: true,
      attention_capability: { kind: 'hook', events: ['turn_completed'], launch_mode: 'skip_permissions', trust: null, min_version: null },
      supports_passive_turn_watcher: false, supports_result_file_handoff: true,
      produces_readable_transcript: true,
      supports_model_override: true, supports_effort_override: true,
      supports_extra_args: true, supports_prefill: true, is_plain_terminal: false,
      effort_control: { kind: 'closed', allowed: ['low', 'medium', 'high'] },
      permission_modes: [], default_permission_mode: null,
      available_on: ['windows', 'macos', 'linux'],
    },
  }] satisfies ProviderInfo[];

  return {
    list_meshes: meshes,
    list_agent_nodes: agentNodes,
    get_default_provider: 'anthropic',
    list_providers: providers,
    list_routing_options: providers,
    list_circuit_agent_ownerships: [] satisfies CircuitAgentOwnership[],
    list_semantic_turns: [] satisfies SemanticTurnPayload[],
    list_circuits_with_runs: circuitsWithRuns,
    list_circuit_queue: circuitQueue,
    count_circuit_run_attention: 1,
    list_circuit_probe: { circuits: circuitsWithRuns, queue: circuitQueue, queue_waits: {} } satisfies CircuitProbeSnapshot,
    get_github_url_for_mesh: null as string | null,
    get_git_branch_status: null as GitBranchStatus | null,
    get_git_summary: { total: 0, added: 0, modified: 0, deleted: 0 } satisfies GitSummary,
    get_app_identifier: 'com.alond.buildmesh.dev',
    log_frontend: null,
    subscribe_agent_output: null,
    resize_agent: null,
    watch_agent_node: null,
    unwatch_agent_node: null,
    get_app_preferences: {
      companion: { launch_configuration: null, spawn_limit: 1 },
      default_provider: 'anthropic', reviewer_provider: null,
      minimax_api_key: null, google_cloud_project: null,
      harness_profiles: [], provider_accounts: [], harness_order: [],
      provider_pairings: [], pairing_verifications: [], proxied_provider_order: [],
      ad0025_account_pairings_migrated: true, naming_provider: null,
      circuit_agent_pool_size: null, circuit_classifier_provider: null,
      harness_defaults: {}, spawn_configurations: [], deleted_launch_configurations: [],
      detached_provider_routes: [], worktree_directory: null, confirm_before_quit: true,
      issue_spawn_prompt: null, pr_spawn_prompt: null,
    } satisfies AppPreferences,
    get_probe_spawn_prompt_defaults: {
      issue_template: 'Implement issue #{{number}}{{title_suffix}}: {{url}}',
      pr_template: '{{policy}}\nReview PR #{{number}}: {{url}}',
      review_policy: 'Review the change and report actionable findings.',
    } satisfies ProbeSpawnPromptDefaults,
    get_preferences_health: { status: 'healthy', corruption: null, preferences_directory: '/home/user/.buildmesh' } satisfies PreferencesHealth,
    get_provider_accounts: [] satisfies ProviderAccount[],
    get_keyed_first_class_catalog: [] satisfies ProviderAccount[],
    get_provider_accounts_with_preferences_keys: [] as string[],
    get_provider_pairings: [] satisfies ProviderPairing[],
    get_pairing_verifications: [] satisfies PairingVerification[],
    compatible_providers_by_harness: {} satisfies Record<string, ProviderAccount[]>,
    get_coordinator_status: { enabled: false, has_token: false } satisfies CoordinatorStatus,
    list_device_sessions: [] satisfies DeviceSession[],
    list_spawn_configurations: [] satisfies SpawnConfiguration[],
    get_launch_targets: [] satisfies LaunchTarget[],
    get_opencode_console_status: {
      signed_in: false, workspaces: [], active_workspace_id: null,
      access_token_expires_at_ms: null, session_expired: false,
    } satisfies OpenCodeConsoleStatus,
    get_network_status: { lan_exposure_enabled: false, port: 1992, tls_active: false, exposed_interfaces: [] } satisfies NetworkStatus,
    auto_resume_agent_nodes: [] as number[],
    is_agent_running: false,
    is_attention_pending: false,
    get_git_status: [] satisfies GitStatus[],
    get_open_pr_for_node: null as OpenPr | null,
    get_mesh_pool_count: 0,
  };
}
