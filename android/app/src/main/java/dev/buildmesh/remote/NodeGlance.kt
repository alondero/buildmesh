package dev.buildmesh.remote

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

/**
 * A Node Glance as the phone reads it from `GET /api/nodes/{id}/glance`: what
 * the agent is doing, how long it has been at it, what it last said and what
 * it was asked. These rules mirror `src/lib/nodeGlance.ts`, which the browser
 * client and the desktop hover card share; `NodeGlanceTest` pins the copy.
 */
data class GlanceActivity(val tool: String, val target: String?) {
    val line: String get() = if (target.isNullOrEmpty()) tool else "$tool · $target"
}

data class NodeGlance(
    val status: String,
    val observedAtMs: Long,
    val statusSinceMs: Long?,
    val sessionStartedAtMs: Long?,
    val createdAtMs: Long,
    val lastPrompt: String?,
    val lastMessage: String?,
    val messageAfterPrompt: Boolean,
    val activity: List<GlanceActivity>,
    val toolCallsSincePrompt: Long,
    val updatedAtMs: Long?,
    val unavailable: String?,
)

private fun JsonObject.optionalText(key: String) = (this[key] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }
private fun JsonObject.optionalNumber(key: String) = (this[key] as? JsonPrimitive)?.longOrNull

fun nodeGlance(body: JsonObject): NodeGlance {
    val transcript = body.objectAt("transcript")
    return NodeGlance(
        status = body.text("status"),
        observedAtMs = body.number("observed_at_ms"),
        statusSinceMs = body.optionalNumber("status_since_ms"),
        sessionStartedAtMs = body.optionalNumber("session_started_at_ms"),
        createdAtMs = body.number("created_at_ms"),
        lastPrompt = transcript?.optionalText("last_prompt"),
        lastMessage = transcript?.optionalText("last_message"),
        messageAfterPrompt = transcript?.flag("message_after_prompt") ?: false,
        activity = transcript?.arrayAt("activity")?.map { GlanceActivity(it.text("tool"), it.optionalText("target")) }.orEmpty(),
        toolCallsSincePrompt = transcript?.number("tool_calls_since_prompt") ?: 0L,
        updatedAtMs = transcript?.optionalNumber("updated_at_ms"),
        unavailable = body.optionalText("transcript_unavailable"),
    )
}

/** Compact elapsed time: `45s`, `12m`, `3h 5m`, `2d 4h`. */
fun formatElapsed(ms: Long): String {
    val seconds = maxOf(0L, ms / 1000)
    if (seconds < 60) return "${seconds}s"
    val minutes = seconds / 60
    if (minutes < 60) return "${minutes}m"
    val hours = minutes / 60
    if (hours < 24) return if (minutes % 60 != 0L) "${hours}h ${minutes % 60}m" else "${hours}h"
    val days = hours / 24
    return if (hours % 24 != 0L) "${days}d ${hours % 24}h" else "${days}d"
}

/**
 * `Running for 12m · session 2h · last activity 8s ago`, measured on the
 * desktop's clock plus how long the phone has held the reading, so the two
 * devices' clocks are never compared.
 */
fun glanceTimingLine(glance: NodeGlance, statusLabel: String, sinceReadMs: Long): String {
    val now = glance.observedAtMs + maxOf(0L, sinceReadMs)
    fun since(at: Long?) = at?.let { formatElapsed(now - it) }
    val inState = since(glance.statusSinceMs)
    return listOfNotNull(
        if (inState != null) "$statusLabel for $inState" else statusLabel,
        since(glance.sessionStartedAtMs ?: glance.createdAtMs)?.let { "session $it" },
        since(glance.updatedAtMs)?.let { "last activity $it ago" },
    ).joinToString(" · ")
}

/** A running node's latest tool call is current work; otherwise it is history. */
fun glanceHeadline(glance: NodeGlance): String? {
    val latest = glance.activity.lastOrNull() ?: return null
    return "${if (glance.status in setOf("running", "spawning")) "Now" else "Last"}: ${latest.line}"
}

fun glanceToolCount(glance: NodeGlance): String? = glance.toolCallsSincePrompt.takeIf { it > 0 }
    ?.let { "$it tool call${if (it == 1L) "" else "s"} since the prompt" }

fun glanceUnavailableCopy(reason: String): String = when (reason) {
    "unsupported" -> "This harness doesn't keep a transcript Buildmesh can read."
    "no_session" -> "No session has been captured yet."
    "no_transcript" -> "No transcript has been written yet."
    "unreadable" -> "The transcript could not be read."
    "empty" -> "Nothing has happened in this session yet."
    "shape_changed" -> "The transcript's format was not recognised."
    else -> "What this agent is doing is not available."
}

/**
 * The node screen's "Right now" card. It is read only while the screen is
 * resumed, on the same five-second cadence as the node list, and its elapsed
 * times tick between reads.
 */
@Composable
fun NodeGlancePanel(api: BuildmeshApi, nodeId: Long) {
    key(api, nodeId) { OwnedNodeGlancePanel(api, nodeId) }
}

@Composable
private fun OwnedNodeGlancePanel(api: BuildmeshApi, nodeId: Long) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var reading by remember { mutableStateOf<Pair<NodeGlance, Long>?>(null) }
    var failed by remember { mutableStateOf(false) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(api, nodeId, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            launch { while (isActive) { now = System.currentTimeMillis(); delay(1_000) } }
            while (true) {
                try {
                    reading = nodeGlance(api.get("/api/nodes/$nodeId/glance").obj()) to System.currentTimeMillis()
                    failed = false
                } catch (e: CancellationException) { throw e }
                // A desktop without the route, or a transient failure, keeps
                // the last reading; the rest of the node screen still works.
                catch (e: Exception) { failed = true }
                delay(5_000)
            }
        }
    }
    val current = reading
    if (current == null) {
        if (failed) Text("Could not read what this agent is doing. Your desktop may need updating.", color = MeshColors.secondary, style = MaterialTheme.typography.bodySmall)
        return
    }
    val (glance, readAt) = current
    Card(colors = CardDefaults.cardColors(containerColor = MeshColors.card), border = BorderStroke(1.dp, MeshColors.borderDefault)) {
        Column(Modifier.fillMaxWidth().padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text("Right now", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Medium)
            Text(glanceTimingLine(glance, MeshStatusTable.resolve(glance.status).label, now - readAt),
                style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
            glanceHeadline(glance)?.let {
                Text(it, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodyMedium, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
            glance.activity.dropLast(1).forEach {
                Text(it.line, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            glanceToolCount(glance)?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary) }
            glance.lastMessage?.let {
                Text(if (glance.messageAfterPrompt) "Last message" else "Last message (earlier task)",
                    style = MaterialTheme.typography.labelMedium, color = MeshColors.secondary, modifier = Modifier.padding(top = 6.dp))
                Text(it, style = MaterialTheme.typography.bodyMedium, maxLines = 10, overflow = TextOverflow.Ellipsis)
            }
            glance.lastPrompt?.let {
                Text("Task", style = MaterialTheme.typography.labelMedium, color = MeshColors.secondary, modifier = Modifier.padding(top = 6.dp))
                Text(it, style = MaterialTheme.typography.bodyMedium, maxLines = 4, overflow = TextOverflow.Ellipsis)
            }
            glance.unavailable?.let { Text(glanceUnavailableCopy(it), style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary) }
            if (failed) Text("Refresh failed; showing the last reading.", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
        }
    }
}
