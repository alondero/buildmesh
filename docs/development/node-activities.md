# Node activities

The desktop node card groups a source agent and its circuit reviewers into activity tabs. The primary agent appears as Implementation when reviewers are present; a standalone agent with a utility tab uses Agent. The status above the tabs describes live agent activity independently of which terminal the user is reading. Concurrent implementation and review are reported together; input requests and errors take precedence.

The existing circuit ledger supplies the relationship through `CircuitAgentOwnership.parent_node_id`. Node-started reviews use the run's borrowed source. The issue-driven review blueprint uses its implementer step. Arbitrary agents sharing a mesh, issue, PR, name, or circuit run are not enough evidence to group them. Custom circuits do not yet expose an authorable activity relationship.

This is presentation, not process ownership. Circuit scheduling, capacity leases, independent worktrees, cancellation, and reviewer cleanup remain owned by their current modules. A closed reviewer disappears from the tab strip; its report remains in the circuit run context under the existing retention policy. A retained reviewer stays inspectable after run completion. If its source is absent or archived, the reviewer is shown as a standalone card.

Grid filtering and pinning operate on individual agents before collapsing matches into cards, so a matching or pinned reviewer keeps its containing card visible. Selecting a review tab sets the active Agent Node to that reviewer: its header actions, input responses, changes, and terminal all target the same agent. Sidebar and attention shortcuts can still select individual agents. Reordering a grouped card moves its root. Closing the selected agent uses the existing individual close workflow; grouping does not introduce cascading deletion.

Build, Run, and Terminal occupy the full card body through a utility tab. The existing backend permits one utility PTY per Agent Node, so choosing a different utility mode replaces that agent's previous utility. Each member can have its own utility tab. Switching back to an agent, changing mesh, or changing grid mode detaches terminal DOM without disposing the process or scrollback. Only the explicit utility close control tears down that utility. Tab selection and open utility modes survive React remounts in `nodeActivityStore`, but are not persisted across app restarts because utility processes do not survive those restarts.

Arrow keys, Home, and End navigate the tab strip. It scrolls horizontally in narrow cards. Agent status is included in every agent tab so attention on an unselected reviewer remains visible.

The regression surface is `tests/unit/node-activities.test.tsx`, grid rendering tests, the existing terminal registry persistence tests, and `db::circuit::activity_ownership_tests` against the production schema and ownership query.
