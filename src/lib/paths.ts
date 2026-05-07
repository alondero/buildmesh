export function getNodeGitPath(node: { path: string; worktree_name?: string }): string {
  if (node.worktree_name) {
    return `${node.path}/.claude/worktrees/${node.worktree_name}`;
  }
  return node.path;
}
