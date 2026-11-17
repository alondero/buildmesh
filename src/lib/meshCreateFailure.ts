/**
 * Why a repository-creation request failed, and what the user can do next.
 *
 * Issue #2794: the New mesh dialog answered a failed creation with
 * "Failed to create mesh. See logs for details." The backend's own message was
 * already available — it was discarded between the store and the dialog — so a
 * new user could not tell whether to choose another folder, fix access, or
 * simply try again. Sending them to the logs at the first meaningful action is
 * an avoidable dead end.
 *
 * This module owns the classification: the store returns the backend's
 * message, and the dialog decides what to offer.
 *
 * The classification is message-driven rather than error-code-driven because
 * `create_mesh` returns `Result<Mesh, String>` — the only structured
 * information the frontend receives is the message text. The phrases below are
 * the ones the operating system, SQLite and the Rust command actually produce.
 *
 * The real message is not rewritten. `message` is the backend's text as it
 * came, so a distinctive rejection ("Access denied creating repository
 * workspace") still reaches the dialog as it was; `hint` is an ADDITION that
 * names the next step.
 */

/** What kind of failure the backend reported. */
export type MeshCreateFailureKind =
  /** The folder is not there, or is not a directory. */
  | 'missing-folder'
  /** The folder is there, but Buildmesh may not use it. */
  | 'access-denied'
  /** The failure may pass on its own. */
  | 'transient'
  /** The message matches no known cause. */
  | 'unknown';

/** The recovery actions the dialog can offer for a failure. */
export type MeshCreateRecovery =
  /** Run the same request again. */
  | 'retry'
  /** Pick a different repository folder. */
  | 'change-folder';

export interface MeshCreateFailure {
  /** The backend's message, verbatim. */
  message: string;
  /** What kind of failure this is. */
  kind: MeshCreateFailureKind;
  /** One short sentence naming the cause and the next step. */
  hint: string;
  /** The recovery actions that make sense for this cause. */
  recoveries: MeshCreateRecovery[];
  /** Diagnostic block for the expandable, copyable detail area. */
  detail: string;
}

/** The operation the diagnostic block names. */
export const MESH_CREATE_OPERATION = 'Open repository folder';

/**
 * The folder is not there. Checked first: a missing path is the failure a user
 * can fix by choosing another folder, and the other causes do not apply once
 * the target itself is gone.
 */
const MISSING_FOLDER_PHRASES: readonly string[] = [
  'cannot find the path',
  'cannot find the file',
  'the system cannot find',
  'no such file',
  'no such directory',
  'os error 2',
  'enoent',
  'does not exist',
  "doesn't exist",
  'not found',
];

/** The folder is there, but Buildmesh was refused access to it. */
const ACCESS_DENIED_PHRASES: readonly string[] = [
  'access is denied',
  'access denied',
  'permission denied',
  'not permitted',
  'read-only file system',
  'read only file system',
  'os error 5',
  'eacces',
  'eperm',
];

/** The failure may pass on its own, so retrying is the reasonable first move. */
const TRANSIENT_PHRASES: readonly string[] = [
  'database is locked',
  'database table is locked',
  'sqlite_busy',
  'resource busy',
  'temporarily unavailable',
  'temporarily',
  'timed out',
  'timeout',
  'connection reset',
  'try again',
  'please retry',
  'too many requests',
  'service unavailable',
];

/** The next step each cause gets. */
const HINTS: Record<MeshCreateFailureKind, string> = {
  'missing-folder':
    'That folder could not be found. Choose the repository folder again.',
  'access-denied':
    'Buildmesh could not use that folder. Fix access to it, or choose a different folder.',
  transient: 'That looks temporary. Try again.',
  unknown: 'The request failed. Try again, or choose a different folder.',
};

function containsAny(haystack: string, needles: readonly string[]): boolean {
  return needles.some((needle) => haystack.includes(needle));
}

/**
 * The diagnostic block behind the expandable, copyable detail area. It names
 * the operation, the folder the user picked and the backend's cause, so a
 * report can be made without the user hunting for the path again.
 */
export function meshCreateFailureDetail(message: string, path: string | null): string {
  const lines = [`Operation: ${MESH_CREATE_OPERATION}`];
  if (path) lines.push(`Folder: ${path}`);
  lines.push(`Cause: ${message}`);
  return lines.join('\n');
}

/**
 * Classify a failed repository creation.
 *
 * @param message the backend's message, verbatim
 * @param path the folder the user picked, when the dialog knows it
 */
export function classifyMeshCreateFailure(
  message: string,
  path: string | null = null
): MeshCreateFailure {
  const trimmed = message.trim();
  const haystack = trimmed.toLowerCase();

  let kind: MeshCreateFailureKind = 'unknown';
  if (containsAny(haystack, MISSING_FOLDER_PHRASES)) {
    kind = 'missing-folder';
  } else if (containsAny(haystack, ACCESS_DENIED_PHRASES)) {
    kind = 'access-denied';
  } else if (containsAny(haystack, TRANSIENT_PHRASES)) {
    kind = 'transient';
  }

  // A bad path or a refused folder is fixed by choosing another folder; a
  // transient failure by trying again. An unclassified cause keeps BOTH safe
  // choices — neither can make things worse, and the real message stays on
  // screen whichever the user picks.
  const recoveries: MeshCreateRecovery[] =
    kind === 'missing-folder' || kind === 'access-denied'
      ? ['change-folder']
      : kind === 'transient'
        ? ['retry']
        : ['retry', 'change-folder'];

  return {
    message: trimmed,
    kind,
    hint: HINTS[kind],
    recoveries,
    detail: meshCreateFailureDetail(trimmed, path),
  };
}
