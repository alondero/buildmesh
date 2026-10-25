# ADR 0047: Backend responsibility modules behind stable entrypoints

Status: accepted

## Context

Issue [#2453](https://github.com/alondero/buildmesh/issues/2453) identified backend
files mixing multiple responsibilities. Earlier extractions already reduced
usage, transcript report snapshots, and worktree provisioning. The remaining
large entrypoints need smaller ownership units without changing runtime behavior,
async offload, connection ownership, IPC shapes, or full Rust test names.

## Decision

Keep the existing entrypoints and move implementations into responsibility
children. The [module map](../development/module-map.md#responsibility-modules)
names each owner. Private helpers use visibility restricted to their owning
module; workflow helpers crossing between GitHub services and command adapters
use `pub(crate)`. No new trait or alternative runtime implementation is needed.

Node creation/import, lifecycle transitions, and durable worktree cleanup have
separate owners. Process construction stays with the process handle; registry
ownership/teardown and input/interrupt orchestration move behind it. Codex setup
separates hook configuration, cached detection, bounded probes, runtime file
access, profiles, and project trust. These are internal seams; spawn ordering,
incarnation fences, and adapter behavior stay in the existing function bodies.

PR command adapters retain offload wrappers and wire DTO declarations. Sync
workflows move to `services/github/` and reuse the existing `GitHubClient` resource
methods in `prs.rs`. The workflows import the existing command DTOs as data
shapes, but do not invoke PR commands. Keeping those declarations preserves
ts-rs export test names as well as generated output. Moving wire types into a
shared model owner is a separate migration with its own test-name consequences.

Worktree primitives separate creation, inclusion, maintenance, safety, and
removal. Shared Git deadlines remain at the entrypoint. The provisioner's
consumer-owned warm claim interface and service-to-Git dependency direction
remain unchanged. Evidence reads, hook receipts, outcomes, waits, transitions,
and effect claims are separate owners; locked helpers still receive the same
connection and capability resolution still runs after releasing the read.

Circuit worker test fragments are included in the existing `tests` scope.
This shards source maintenance by subdomain while retaining libtest names,
shared fixtures, and the existing CI process isolation. It does not create
separate Rust test binaries. Scoped-check mappings select the original contract
suite when an extracted implementation or included test file changes.

## Alternatives and consequences

A line-count-only split would leave responsibilities mixed. New public services
or wrapper traits would enlarge the interface without a second implementation.
Moving tests into new nested modules would change full test names and existing
filters. Instead, keep the entrypoints and tests stable and move function bodies
with import, path, and visibility adjustments. No frontend or user-guide change
is needed because product behavior and wire shapes are unchanged.

## Verification

Compare the baseline and candidate `cargo test -- --list` inventories and generated
TypeScript bytes. Run the Rust shard suites, shard coverage, all-target Clippy,
and repository verification. Existing source guards must scan the new owning
files; scoped-check tests must prove extracted files select their contract suites.
