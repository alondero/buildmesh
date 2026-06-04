export function getNodeGitPath(node: { path: string; worktree_name?: string; use_worktree?: boolean }): string {
  if (node.use_worktree !== false && node.worktree_name) {
    return `${node.path}/.claude/worktrees/${node.worktree_name}`;
  }
  return node.path;
}
