// Client-side mirror of the backend `parse_clone_input` rules
// (`src-tauri/src/services/github/sync.rs`). Used only to drive the clone
// form's destination preview and its enable/disable gate. The accepted forms
// are deliberately the *exact* ones the backend takes (literal, case-sensitive
// prefixes) — a looser gate here would light up Clone for input the backend
// rejects, e.g. `http://` or `git://` github.com URLs.

/** The github.com remote prefixes the backend recognises, in its order. */
const ACCEPTED_PREFIXES = [
  'https://github.com/',
  'git@github.com:',
  'ssh://git@github.com/',
] as const;

/** A GitHub repo name is a single path segment — same guard as the backend's
 * `is_valid_repo_name`, so `.`/`..` or a separator can't reach the preview. */
function isValidRepoName(name: string): boolean {
  return !!name && name !== '.' && name !== '..' && !/[/\\:\s]/.test(name);
}

function repoFromPath(pathPart: string): string | null {
  const segments = pathPart.split('/').filter(Boolean);
  if (segments.length < 2) return null;
  const repo = segments[1].replace(/\.git$/, '');
  return isValidRepoName(repo) ? repo : null;
}

/** Repo name from `owner/repo` or a github.com URL the backend accepts, or
 * `null` when the input isn't (yet) a recognizable GitHub repository. */
export function repoNameFromInput(input: string): string | null {
  const trimmed = input.trim().replace(/\/+$/, '');
  if (!trimmed) return null;

  const prefix = ACCEPTED_PREFIXES.find((p) => trimmed.startsWith(p));
  if (prefix) {
    return repoFromPath(trimmed.slice(prefix.length));
  }

  // Any other URL-ish input must not fall through to the bare shorthand: the
  // backend only treats a bare `owner/repo` (no scheme, no userinfo) as valid.
  if (trimmed.includes('://') || trimmed.includes('@')) return null;

  // Bare `owner/repo`: exactly one separator, no scheme/path noise.
  if (/[\s\\:]/.test(trimmed)) return null;
  const segments = trimmed.split('/');
  if (segments.length !== 2) return null;
  const repo = segments[1].replace(/\.git$/, '');
  return segments[0] && isValidRepoName(repo) ? repo : null;
}

/** Join a parent path and a child name using the separator already present in
 * the parent, so the preview matches the path the backend will store. */
export function joinDisplayPath(parent: string, name: string): string {
  const separator = parent.includes('\\') ? '\\' : '/';
  return `${parent.replace(/[\\/]+$/, '')}${separator}${name}`;
}
