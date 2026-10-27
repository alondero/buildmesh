package dev.buildmesh.remote

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject

/** The nodes this deck is about: every open agent blocked on a person. */
fun waitingForInput(nodes: List<JsonObject>): List<JsonObject> =
    nodes.filter { nodeInputRequest(it) != null }.sortedBy { it.number("id") }

/**
 * The agents that are waiting on you, pinned above everything else.
 *
 * This is the phone's most-used surface, so it is the first thing on the home
 * screen: what the agent asked, which mesh and branch it is, and the one action
 * the harness actually offers. The reply controls are chosen from the observed
 * request, never from the bare `awaiting_input` status (issue #1966) — an
 * approval the harness asked for gets the desktop semantic-turn banner's own
 * `Allow (Y)` / `Deny (N)` pair, sending the same `y\r` / `n\r` keystrokes, and
 * anything less verified gets an explicit way into the terminal instead of a
 * keystroke Buildmesh cannot be sure the harness accepts.
 *
 * Presentational: the screen owns the reply transport, so the deck is testable
 * without a ViewModel. `answer` reports whether the request reached the
 * transport and completes with `true` once the desktop accepted it, or `false`
 * when the send failed — so a card is retired only by a reply the desktop
 * took, and a failed reply hands the controls back instead of leaving them
 * disabled.
 */
@Composable
fun AttentionDeck(
    nodes: List<JsonObject>,
    meshes: List<JsonObject>,
    busy: Boolean,
    openNode: (Long) -> Unit,
    openTerminal: (Long) -> Unit,
    answer: (nodeId: Long, sequence: String, onDone: (accepted: Boolean) -> Unit) -> Boolean,
) {
    val waiting = waitingForInput(nodes)
    if (waiting.isEmpty()) return
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Waiting for input", style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Medium, color = MeshColors.amber)
        waiting.forEach { node -> AttentionCard(node, meshes, busy, openNode, openTerminal, answer) }
    }
}

@Composable
private fun AttentionCard(
    node: JsonObject,
    meshes: List<JsonObject>,
    busy: Boolean,
    openNode: (Long) -> Unit,
    openTerminal: (Long) -> Unit,
    answer: (nodeId: Long, sequence: String, onDone: (accepted: Boolean) -> Unit) -> Boolean,
) {
    val request = nodeInputRequest(node) ?: return
    val id = node.number("id")
    val mesh = meshes.firstOrNull { it.number("id") == node.number("mesh_id") }
    // The marker belongs to *this* request, not to the node, so it is keyed by
    // the request: a node that answers one prompt and asks another arrives with
    // live controls rather than this one's disabled pair.
    var pending by remember(request.key) { mutableStateOf("") }
    var accepted by remember(request.key) { mutableStateOf("") }
    // Any request in flight anywhere blocks these: one reply per decision, and
    // `answer` refuses a second one anyway.
    val blocked = busy || pending.isNotEmpty() || accepted.isNotEmpty()
    fun reply(sequence: String, action: String) {
        // Mark in flight *before* asking the transport, so a failure signal
        // that lands immediately cannot be overwritten by the marker; the
        // refusal path below clears it again.
        pending = action
        if (!answer(id, sequence, { ok -> if (ok) accepted = action else pending = "" })) pending = ""
    }
    Card(
        onClick = { openNode(id) },
        colors = CardDefaults.cardColors(containerColor = MeshColors.card),
        border = BorderStroke(1.dp, MeshColors.attentionRing),
    ) {
        Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                WorkStatusGlyph(node, null)
                Column(Modifier.weight(1f)) {
                    Text(node.text("name"), style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(nodeLabel(node), style = MaterialTheme.typography.bodySmall, color = MeshColors.amber)
                }
            }
            Text(
                listOf(mesh?.text("name").orEmpty().ifEmpty { "Unknown mesh" }, node.text("branch"))
                    .filter { it.isNotEmpty() }.joinToString(" · "),
                style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            Text(
                request.message.ifEmpty { "Waiting for the agent's prompt…" },
                style = MaterialTheme.typography.bodySmall, color = MeshColors.amber,
                maxLines = 3, overflow = TextOverflow.Ellipsis,
            )
            // The answers the harness enumerated, as text: a tap here cannot be
            // verified to select the option in the agent's own prompt.
            request.choices.forEach { choice ->
                Text("• $choice", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (request.isPermission) {
                    ReplyChip(
                        label = when {
                            accepted == "allow" -> request.approvedLabel
                            pending == "allow" -> "Sending…"
                            else -> request.approveLabel
                        },
                        content = MeshColors.green, border = MeshColors.approveBorder,
                        enabled = !blocked, modifier = Modifier.weight(1f),
                    ) { reply("y\r", "allow") }
                    ReplyChip(
                        label = when {
                            accepted == "deny" -> request.rejectedLabel
                            pending == "deny" -> "Sending…"
                            else -> request.rejectLabel
                        },
                        content = MeshColors.red, border = MeshColors.rejectBorder,
                        enabled = !blocked, modifier = Modifier.weight(1f),
                    ) { reply("n\r", "deny") }
                } else {
                    ReplyChip(
                        label = if (request.mode == "question") "Answer in terminal" else "Open terminal to respond",
                        content = MeshColors.secondary, border = MeshColors.borderStrong,
                        enabled = true, modifier = Modifier.weight(1f),
                    ) { openTerminal(id) }
                }
            }
        }
    }
}

/** One hairline reply chip: the phone web client's `.deck-chip`, hue-bordered. */
@Composable
private fun ReplyChip(
    label: String,
    content: Color,
    border: Color,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    OutlinedButton(
        onClick = onClick,
        enabled = enabled,
        shape = MaterialTheme.shapes.small,
        colors = ButtonDefaults.outlinedButtonColors(contentColor = content),
        border = BorderStroke(1.dp, border),
        contentPadding = PaddingValues(horizontal = 8.dp),
        modifier = modifier,
    ) {
        Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}
