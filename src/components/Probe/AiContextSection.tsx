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
      <div className="rounded border border-green-400/30 bg-green-400/10 p-3">
        <p className="text-xs text-green-400 font-medium mb-1">Portability PR created!</p>
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
    <div className="rounded border border-[#2a2a2a] p-3 space-y-2">
      <p className="text-xs font-medium text-[#e0e0e0]">AI context portability</p>

      <div className="space-y-0.5 text-[11px] text-[#9ca3af]">
        {status.claude_md_exists && (
          <div className="flex items-center gap-2">
            <span className="font-mono text-[#d1d5db]">CLAUDE.md</span>
            <span>→</span>
            <span className="font-mono text-[#d1d5db]">AGENTS.md</span>
            {status.agents_md_exists ? (
              <span className="text-green-400">✓</span>
            ) : (
              <span className="text-[#6b7280]">(will create)</span>
            )}
          </div>
        )}
        {status.skills_dir_exists && (
          <div className="flex items-center gap-2">
            <span className="font-mono text-[#d1d5db]">.claude/skills</span>
            <span>→</span>
            <span className="font-mono text-[#d1d5db]">.agents/skills</span>
            {status.agents_skills_exists ? (
              <span className="text-green-400">✓</span>
            ) : (
              <span className="text-[#6b7280]">(will create, {status.skill_count} skills)</span>
            )}
          </div>
        )}
      </div>

      {!needsWork ? (
        <p className="text-[10px] text-green-400">Already portable — Codex, OpenCode &amp; Antigravity can read this context.</p>
      ) : (
        <>
          <p className="text-[10px] text-[#6b7280] leading-snug">
            Opens a PR adding the above as git symlinks. Note: a symlink checked out on Windows
            without Developer Mode becomes a plain text file; macOS/Linux and Windows+Dev Mode
            resolve it correctly.
          </p>
          {error && <p className="text-[10px] text-red-400">{error}</p>}
          {!isAuthenticated ? (
            <span className="text-[10px] text-[#ef4444]">Run `gh auth login` first</span>
          ) : (
            <button
              onClick={handleCreate}
              disabled={submitting}
              className="w-full bg-accent-cyan/10 hover:bg-accent-cyan/20 text-accent-cyan text-xs py-1.5 rounded transition-colors disabled:opacity-50"
            >
              {submitting ? 'Creating PR…' : 'Make AI context portable'}
            </button>
          )}
        </>
      )}
    </div>
  );
}
