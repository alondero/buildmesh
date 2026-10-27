package dev.buildmesh.remote

import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.AnchoredDraggableState
import androidx.compose.foundation.gestures.DraggableAnchors
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.anchoredDraggable
import androidx.compose.foundation.gestures.animateTo
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.LayoutDirection
import dev.buildmesh.remote.wire.CircuitAgentOwnership
import dev.buildmesh.remote.wire.NodeCircuitRun
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlin.math.roundToInt

data class WorkActivity(val root: JsonObject, val members: List<JsonObject>)

// Who needs a person, most urgent first: a failed agent, then a Circuit run
// waiting on you, then an agent blocked on its own input. The order is
// deliberate — a failed agent needs a retry, a Circuit gate needs a decision,
// and an awaiting agent needs an answer.
private enum class WorkNeed { Failed, Circuit, Input, None }

private fun workNeed(node: JsonObject, runs: List<NodeCircuitRun>): WorkNeed = when {
    node.text("status") in setOf("error", "lost") -> WorkNeed.Failed
    runs.any { it.node_id == node.number("id") && it.needs_you.isNotEmpty() } -> WorkNeed.Circuit
    node.text("status") == "awaiting_input" -> WorkNeed.Input
    else -> WorkNeed.None
}

// Follow the sidebar's Circuit lineage, stopping at missing/archived parents,
// mesh boundaries and cycles so an invalid relationship cannot hide an agent.
fun workActivities(nodes: List<JsonObject>, ownerships: List<CircuitAgentOwnership>): List<WorkActivity> {
    val open = nodes.filter { it.text("status") != "archived" }
    val index = open.associateBy { it.number("id") }
    val owners = ownerships.associateBy { it.node_id }
    fun root(node: JsonObject): Long {
        val seen = mutableSetOf<Long>()
        var current = node
        while (seen.add(current.number("id"))) {
            val parent = owners[current.number("id")]?.parent_node_id?.let(index::get) ?: return current.number("id")
            if (parent.number("mesh_id") != current.number("mesh_id")) return current.number("id")
            current = parent
        }
        return node.number("id")
    }
    val order = compareBy<JsonObject> { it.number("position") }.thenBy { it.number("id") }
    return open.groupBy(::root).map { (id, members) ->
        val representative = index.getValue(id)
        WorkActivity(representative, listOf(representative) + members.filter { it.number("id") != id }.sortedWith(order))
    }.sortedWith(compareBy<WorkActivity> { it.root.number("position") }.thenBy { it.root.number("id") })
}

fun matchingWorkActivities(state: RemoteState, meshId: Long, query: String, attentionOnly: Boolean): List<WorkActivity> =
    workActivitiesByNeed(
        workActivities(state.nodes, state.circuitOwnerships).filter { activity ->
            (meshId == 0L || activity.root.number("mesh_id") == meshId) && activity.members.any { node ->
                (!attentionOnly || circuitNeedsAttention(node, state.circuitRuns)) &&
                    (query.isBlank() || node.text("name").contains(query, true) || node.text("branch").contains(query, true))
            }
        },
        state.circuitRuns)

// The Work tab lists a mesh's activities with whoever needs a person first
// (issue #2613), so an agent blocked on a decision no longer sits below the
// running ones. The bracket moves as a unit: its representative stays first
// and its members keep their order, and no activity leaves its mesh group.
// Everything that needs nobody keeps the desktop's saved order.
fun workActivitiesByNeed(activities: List<WorkActivity>, runs: List<NodeCircuitRun>): List<WorkActivity> =
    activities.groupBy { it.root.number("mesh_id") }.values.flatMap { mesh ->
        mesh.sortedBy { activity -> activity.members.minOfOrNull { workNeed(it, runs) } ?: WorkNeed.None }
    }

@Composable
fun WorkMeshHeader(mesh: JsonObject?, count: Int) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(mesh?.text("name") ?: "Unknown mesh", Modifier.weight(1f),
            style = MaterialTheme.typography.titleSmall, color = MeshColors.secondary,
            maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text("$count", style = MaterialTheme.typography.labelMedium, color = MeshColors.muted)
    }
}

@Composable
fun WorkActivityRows(activity: WorkActivity, state: RemoteState, motion: WorkGlyphMotion, openNode: (Long) -> Unit,
    closeNode: ((Long) -> Unit)? = null) {
    val paired = activity.members.size > 1
    val tone = when {
        activity.members.any { it.text("status") in setOf("error", "lost") } -> MeshColors.red
        activity.members.any { circuitNeedsAttention(it, state.circuitRuns) } -> MeshColors.amber
        activity.members.any { it.text("status") in setOf("running", "pending", "spawning") } -> MeshColors.accent
        else -> MeshColors.accent.copy(alpha = 0.4f)
    }
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    val bracket = if (paired) Modifier.semantics {
        contentDescription = "Paired group of ${activity.members.size} agents"
    }.drawBehind {
        val x = if (rtl) size.width - 4.dp.toPx() else 4.dp.toPx()
        val inset = 10.dp.toPx()
        val cap = 4.dp.toPx() * if (rtl) -1 else 1
        val stroke = 2.dp.toPx()
        drawLine(tone, Offset(x, inset), Offset(x, size.height - inset), stroke)
        drawLine(tone, Offset(x, inset), Offset(x + cap, inset), stroke)
        drawLine(tone, Offset(x, size.height - inset), Offset(x + cap, size.height - inset), stroke)
    } else Modifier
    Column(Modifier.fillMaxWidth().then(bracket)) {
        activity.members.forEach { node ->
            key(node.number("id")) {
                val id = node.number("id")
                val owner = state.circuitOwnerships.firstOrNull { it.node_id == id }
                WorkAgentRow(node, state.busy, openNode, closeNode,
                    modifier = if (paired) Modifier.padding(start = 10.dp) else Modifier) {
                    WorkStatusGlyph(node, owner, motion)
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        Text(node.text("name"), style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Medium,
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                        val detail = listOf(nodeLabel(node), node.text("branch")).filter { it.isNotBlank() }.joinToString(" · ")
                        Text(detail, style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary,
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                        state.circuitRuns.firstOrNull { it.node_id == id }?.takeIf { it.needs_you.isNotEmpty() }?.let {
                            Text(circuitStatus(it), style = MaterialTheme.typography.bodySmall, color = MeshColors.amber,
                                maxLines = 2, overflow = TextOverflow.Ellipsis)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun WorkAgentRow(node: JsonObject, busy: Boolean, openNode: (Long) -> Unit,
    closeNode: ((Long) -> Unit)?, modifier: Modifier = Modifier, content: @Composable RowScope.() -> Unit) {
    val id = node.number("id")
    val name = node.text("name")
    val width = with(LocalDensity.current) { 88.dp.toPx() }
    val revealOffset = if (LocalLayoutDirection.current == LayoutDirection.Rtl) width else -width
    val swipe = remember(id, revealOffset) {
        AnchoredDraggableState(false, DraggableAnchors { false at 0f; true at revealOffset })
    }
    val scope = rememberCoroutineScope()
    Box(modifier.fillMaxWidth().clipToBounds()) {
        // Only expose the action to touch and accessibility after revealing it.
        if (swipe.offset != 0f && closeNode != null) {
            Row(Modifier.matchParentSize(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = { closeNode(id) }, enabled = !busy && swipe.settledValue,
                    modifier = Modifier.width(88.dp).fillMaxHeight().background(MaterialTheme.colorScheme.errorContainer),
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onErrorContainer)) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(Icons.Default.Close, null)
                        Text("Close")
                    }
                }
            }
        }
        Row(Modifier.absoluteOffset { IntOffset(swipe.requireOffset().roundToInt(), 0) }
            .fillMaxWidth().background(MeshColors.background)
            .anchoredDraggable(swipe, orientation = Orientation.Horizontal, enabled = !busy && closeNode != null,
                reverseDirection = false)
            .semantics {
                if (!busy && closeNode != null) customActions = listOf(
                    CustomAccessibilityAction("Close $name") { closeNode(id); true }
                )
            }
            .clickable(role = Role.Button, onClickLabel = "Open $name") {
                if (swipe.offset != 0f) scope.launch { swipe.animateTo(false) } else openNode(id)
            }
            .heightIn(min = 56.dp).padding(start = 12.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
            content = content)
    }
}
