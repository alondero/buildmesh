# ADR 0039: Desktop readiness, related tools, and lifecycle finding

Status: accepted.

## Context

The October desktop audit found that setup guidance, hidden filters, and
repository grouping made the next action difficult to find. Related tools
already have grouped discovery in the omnibar; the permanent activity rail
assumed by older proposals no longer exists. Legacy Autopilot Policies have
also been removed. Circuits remains the supported automation destination.

## Decision

The empty workspace shows three skippable readiness steps: repository,
harness/runtime/login, and a first session. Local harness checks are distinct
from authentication; an available CLI does not prove login. A Terminal remains
available without agent credentials. Advanced shortcuts live behind Help.

Filtered mode keeps its compact search and adds a popover with all persisted
controls, result counts, removable filter chips and a single Clear all action.
Workspace order remains stable. A separate sidebar Attention view lists failed,
waiting and suspended nodes across repositories, with direct terminal/recovery
actions; it never changes the workspace's drag order.

Files & changes and GitHub each occupy one visited-tool strip slot. Their
subviews preserve the original destination IDs, scope, baselines and per-view
pins. Switching subviews does not silently carry an Agent pin into a repository
view. The header identifies subject and pin mode; the subview toolbar identifies
the Files comparison. Omnibar commands and contextual deep links continue to
open the exact requested subview. The last Files subview survives restart.

Agent History is a Host destination, with an explicit repository filter,
lifecycle search and repository-scoped discovered sessions. The read includes
archived database rows as well as live work. Reopen changes only archived nodes
to Suspended, preserving node identity, provider and captured session; it does
not spawn. Resume is a separate, explicit action. Missing/deleted subjects and
failed reads report errors rather than an invented empty collection.

Routing defaults use a cheap preferences-derived catalog with cached startup
installation information, independently of live provider probes. OpenAI routes
and their saved configurations explain pending verification and remain disabled
until the live menu validates them. Preferences must load before any picker
can save. Live probe failures retain Retry and never erase the cheap choices.

## Alternatives

Reusing the warm full-provider snapshot reduces repeat latency but leaves cold
Settings loads dependent on unrelated subprocesses. Replacing destination IDs
with new parent IDs would break existing commands and require pin migration.
Both alternatives add risk without improving this pass's acceptance behavior.

## Consequences and migration

No database schema or agent lifecycle semantics change. Existing destination
commands remain valid. Inspector working sets and pins are session-only; old
Agent History pins no longer apply to its Host finder. Existing filter storage
is read unchanged. Grouping is a presentation over the existing working set;
its four-group eviction limit remains intact. Project Settings and
Repository retain their separate configuration and maintenance roles.

## Verification

Behavioral tests cover routing-before-probes, preferences failure safety,
keyboard tabs, combined filters/reset, recovery, archived reads and reopening.
The [October audit](../development/desktop-ux-audit-2026-10.md) records desktop
screenshots and runtime evidence. See the [user guide](../user-guide.md) for
the resulting navigation and readiness flow.
