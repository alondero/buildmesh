# Issue Tracker

GitHub Issues are used for tracking work on this repo.

- **Repository**: `alondero/buildmesh`
- **CLI**: `gh` (GitHub CLI)

## Usage by Skills

| Skill | Action |
|-------|--------|
| `to-issues` | Creates GitHub issues via `gh issue create` |
| `triage` | Applies labels via `gh issue edit --add-label` |
| `to-prd` | Creates GitHub issues and PRs |
| `review` | Posts review comments via `gh pr comment` |

## Consumer Rules

- Skills read issues using `gh issue list` and `gh issue view`
- Skills write issues using `gh issue create`
- Labels are applied using `gh issue edit --add-label`
- All issue operations use the default GitHub remote (`origin`)
