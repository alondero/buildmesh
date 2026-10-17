package dev.buildmesh.remote

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.buildmesh.remote.wire.CircuitAgentOwnership
import kotlinx.serialization.json.JsonObject

data class WorkActivity(val root: JsonObject, val members: List<JsonObject>)

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
    workActivities(state.nodes, state.circuitOwnerships).filter { activity ->
        (meshId == 0L || activity.root.number("mesh_id") == meshId) && activity.members.any { node ->
            (!attentionOnly || circuitNeedsAttention(node, state.circuitRuns)) &&
                (query.isBlank() || node.text("name").contains(query, true) || node.text("branch").contains(query, true))
        }
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
fun WorkActivityRows(activity: WorkActivity, state: RemoteState, motion: WorkGlyphMotion, openNode: (Long) -> Unit) {
    val paired = activity.members.size > 1
    val tone = when {
        activity.members.any { it.text("status") in setOf("error", "lost") } -> MeshColors.red
        activity.members.any { circuitNeedsAttention(it, state.circuitRuns) } -> MeshColors.amber
        activity.members.any { it.text("status") in setOf("running", "pending", "spawning") } -> MeshColors.accent
        else -> MeshColors.accent.copy(alpha = 0.4f)
    }
    val bracket = if (paired) Modifier.semantics {
        contentDescription = "Paired group of ${activity.members.size} agents"
    }.drawBehind {
        val x = 4.dp.toPx()
        val inset = 10.dp.toPx()
        val cap = 4.dp.toPx()
        val stroke = 2.dp.toPx()
        drawLine(tone, Offset(x, inset), Offset(x, size.height - inset), stroke)
        drawLine(tone, Offset(x, inset), Offset(x + cap, inset), stroke)
        drawLine(tone, Offset(x, size.height - inset), Offset(x + cap, size.height - inset), stroke)
    } else Modifier
    Column(Modifier.fillMaxWidth().then(bracket)) {
        activity.members.forEach { node ->
            val id = node.number("id")
            val owner = state.circuitOwnerships.firstOrNull { it.node_id == id }
            Row(Modifier.fillMaxWidth().clickable(role = Role.Button, onClickLabel = "Open ${node.text("name")}") { openNode(id) }
                .heightIn(min = 56.dp).padding(start = 12.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
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
