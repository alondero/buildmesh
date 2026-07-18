import { useEffect, useState } from 'react';
import {
  detectAiContext,
  createAiContextPortabilityPr,
  type AiContextStatus,
} from '../../lib/tauri';

interface AiContextSectionProps {
  meshId: number;
  meshPath: string;
  isAuthenticated: boolean;
}

/**
 * Detects a project's Claude AI context (CLAUDE.md / .claude/skills) and offers
 * to open a PR mirroring it as AGENTS.md + .agents/skills symlinks, so Codex,
 * OpenCode and Antigravity read the same context.
 */
export function AiContextSection({ meshId, meshPath, isAuthenticated }: AiContextSectionProps) {
  const [status, setStatus] = useState<AiContextStatus | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [prUrl, setPrUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setStatus(null);
    detectAiContext(meshPath)
      .then((s) => alive && setStatus(s))
      .catch(() => alive && setStatus(null));
    return () => {
      alive = false;
    };
  }, [meshPath]);

  if (!status) return null;

  const hasClaude = status.claude_md_exists || status.skills_dir_exists;
  const needsAgentsMd = status.claude_md_exists && !status.agents_md_exists;
  const needsAgentsSkills = status.skills_dir_exists && !status.agents_skills_exists;
  const needsWork = needsAgentsMd || needsAgentsSkills;

  // Nothing Claude-shaped to port — keep the panel quiet.
  if (!hasClaude) return null;

  const handleCreate = async () => {
    setSubmitting(true);
    setError(null);
    try {
      const url = await createAiContextPortabilityPr(meshId);
      setPrUrl(url);
    } catch (e) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  if (prUrl) {
    return (
      <div className="rounded-md border border-status-success/30 bg-status-success-bg p-3">
        <p className="text-xs text-status-success font-medium mb-1">Portability PR created!</p>
        <a
          href={prUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="text-xs text-accent-cyan hover:underline break-all"
        >
          {prUrl}
        </a>
      </div>
    );
  }

  return (
    <div className="rounded-md border border-border-default p-3 space-y-2">
      <p className="text-xs font-medium text-text-primary">AI context portability</p>

      <div className="space-y-0.5 text-xs text-text-secondary">
        {status.claude_md_exists && (
          <div className="flex items-center gap-2">
            <span className="font-mono text-text-primary">CLAUDE.md</span>
            <span>→</span>
            <span className="font-mono text-text-primary">AGENTS.md</span>
            {status.agents_md_exists ? (
              <span className="text-status-success">✓</span>
            ) : (
              <span className="text-text-muted">(will create)</span>
            )}
          </div>
        )}
        {status.skills_dir_exists && (
          <div className="flex items-center gap-2">
            <span className="font-mono text-text-primary">.claude/skills</span>
            <span>→</span>
            <span className="font-mono text-text-primary">.agents/skills</span>
            {status.agents_skills_exists ? (
              <span className="text-status-success">✓</span>
            ) : (
              <span className="text-text-muted">(will create, {status.skill_count} skills)</span>
            )}
          </div>
        )}
      </div>

      {!needsWork ? (
        <p className="text-2xs text-status-success">Already portable — Codex, OpenCode &amp; Antigravity can read this context.</p>
      ) : (
        <>
          <p className="text-2xs text-text-muted leading-snug">
            Opens a PR adding the above as git symlinks. Note: a symlink checked out on Windows
            without Developer Mode becomes a plain text file; macOS/Linux and Windows+Dev Mode
            resolve it correctly.
          </p>
          {error && <p className="text-2xs text-status-error">{error}</p>}
          {!isAuthenticated ? (
            <span className="text-2xs text-status-error">Run `gh auth login` first</span>
          ) : (
            <button
              onClick={handleCreate}
              disabled={submitting}
              className="w-full bg-accent-cyan/10 hover:bg-accent-cyan/20 text-accent-cyan text-xs py-1.5 rounded-md transition-colors disabled:opacity-50"
            >
              {submitting ? 'Creating PR…' : 'Make AI context portable'}
            </button>
          )}
        </>
      )}
    </div>
  );
}
