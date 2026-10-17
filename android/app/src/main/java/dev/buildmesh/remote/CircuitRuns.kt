package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.buildmesh.remote.wire.*
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.*

suspend fun BuildmeshApi.circuitRuns(): List<NodeCircuitRun> {
    val body = get("/api/circuit-runs")
    if (body is JsonObject && body.text("error") == "not found") return emptyList()
    return body.jsonArray.map(NodeCircuitRun::fromJson)
}

suspend fun BuildmeshApi.circuitOwnerships(): List<CircuitAgentOwnership> {
    val body = try { get("/api/circuit-agent-ownerships") }
    catch (error: ApiException) { if (error.status == 404) return emptyList() else throw error }
    if (body is JsonObject && body.text("error") == "not found") return emptyList()
    return body.jsonArray.map(CircuitAgentOwnership::fromJson)
}

suspend fun BuildmeshApi.reviewOptions(nodeId: Long): List<NodeReviewOption> {
    val body = get("/api/nodes/$nodeId/reviews")
    if (body is JsonObject && body.text("error") == "not found") return emptyList()
    return body.jsonArray.map(NodeReviewOption::fromJson)
}

private val stateLabels = mapOf("pending" to "Queued", "running" to "Running", "paused" to "Paused", "failed" to "Failed", "completed" to "Completed", "cancelled" to "Cancelled")
private val stepLabels = mapOf("blocked" to "Needs approval", "unverified" to "Unverified Checkpoint", "pending_slot" to "Queued", "queued" to "Queued", "running" to "Running", "completed" to "Done", "pending" to "Queued", "failed" to "Failed", "cancelled" to "Cancelled")
private val verdictLabels = mapOf("approved" to "Approved", "changes_requested" to "Changes requested", "blocked" to "Needs attention")
private val checkpointLabels = mapOf("recheck" to "Check again", "review_pass" to "Review passed", "review_fail" to "Request changes", "completed" to "I've done this — mark it done", "not_performed" to "It didn't happen", "retry" to "Try it again")

fun circuitStatus(run: NodeCircuitRun): String = buildList {
    add("${if (run.needs_you.isNotEmpty()) "Needs you" else stateLabels[run.state] ?: run.state}: ${run.blueprint_name}")
    run.review?.let {
        add("Round ${it.round}" + (it.max_rounds?.let { max -> " of $max" } ?: ""))
        it.verdict?.let { verdict -> add(verdictLabels[verdict] ?: verdict) }
    }
    run.step?.let { add("${it.role}: ${stepLabels[it.status] ?: it.status}") }
}.joinToString(" · ")

fun circuitNeedsAttention(node: JsonObject, runs: List<NodeCircuitRun>): Boolean =
    node.text("status") == "awaiting_input" || runs.any { it.node_id == node.number("id") && it.needs_you.isNotEmpty() }

data class CircuitDecision(val runId: Long, val revision: Long, val kind: String, val stepId: String = "", val attempt: Long = 0, val action: String = "")

fun decisionOffered(decision: CircuitDecision, run: NodeCircuitRun): Boolean {
    if (decision.runId != run.run_id || decision.revision != run.revision) return false
    return when (decision.kind) {
        "cancel" -> run.state in setOf("pending", "running", "paused")
        "recover" -> run.recovery?.let { recovery ->
            recovery.node_id == decision.stepId && recovery.attempt == decision.attempt &&
                recovery.options.any { it.action == decision.action && it.available }
        } ?: false
        else -> run.needs_you.any { need ->
            need.step.node_id == decision.stepId && need.step.attempt == decision.attempt &&
                if (decision.kind == "approve") need.kind == "approval" else need.kind == "unverified" && decision.action in need.actions
        }
    }
}

suspend fun BuildmeshApi.actOnCircuit(decision: CircuitDecision, reason: String) {
    val path = "/api/circuit-runs/${decision.runId}"
    when (decision.kind) {
        "approve" -> post("$path/approve", ApproveCircuitStepRequest(decision.stepId).toJson())
        "cancel" -> post("$path/cancel", buildJsonObject {})
        "recover" -> post("$path/recover", RecoveryRequest(decision.runId, decision.stepId, decision.attempt, decision.revision, decision.action, reason).toJson())
        "outcome" -> post("$path/outcome", CheckpointRequest(decision.runId, decision.stepId, decision.attempt, decision.revision, decision.action, reason).toJson())
        else -> error("Unknown Circuit decision")
    }
}

@Composable
fun CircuitRunPanel(run: NodeCircuitRun, vm: BuildmeshViewModel, openTerminal: (Long) -> Unit) {
    val state by vm.state.collectAsState()
    var decision by remember(run.run_id) { mutableStateOf<CircuitDecision?>(null) }
    var reason by remember(run.run_id) { mutableStateOf("") }
    fun choose(kind: String, step: NodeCircuitStep? = null, action: String = "") {
        vm.dismissError()
        reason = ""
        decision = CircuitDecision(run.run_id, run.revision, kind, step?.node_id.orEmpty(), step?.attempt ?: 0, action)
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Circuit Run", style = MaterialTheme.typography.titleMedium)
        Text(circuitStatus(run), color = if (run.needs_you.isNotEmpty()) MeshColors.amber else MeshColors.secondary)
        run.review?.summary?.let { Text(it) }
        run.needs_you.forEach { need ->
            Text(when (need.kind) {
                "approval" -> "${need.step.role} needs your approval"
                "input" -> "${need.step.role} needs your answer"
                "unverified" -> "Unverified Checkpoint: ${need.step.role}"
                else -> "Failed: ${need.step.role}"
            })
            need.detail?.let { Text(it, color = MeshColors.secondary) }
            when (need.kind) {
                "approval" -> OutlinedButton(enabled = !state.busy, onClick = { choose("approve", need.step) }) { Text("Approve…") }
                "input" -> need.agent_node_id?.let { id -> OutlinedButton(onClick = { openTerminal(id) }) { Text("Open terminal to answer") } }
                "unverified" -> need.actions.forEach { action ->
                    OutlinedButton(enabled = !state.busy, onClick = {
                        if (action == "recheck") vm.action { it.actOnCircuit(CircuitDecision(run.run_id, run.revision, "outcome", need.step.node_id, need.step.attempt, action), "Checked again from the phone.") }
                        else choose("outcome", need.step, action)
                    }) { Text(checkpointLabels[action] ?: action) }
                }
            }
        }
        run.recovery?.let { recovery ->
            Text("Run Recovery", style = MaterialTheme.typography.titleSmall)
            recovery.options.forEach { option ->
                if (option.available) OutlinedButton(enabled = !state.busy, onClick = {
                    val next = CircuitDecision(run.run_id, run.revision, "recover", recovery.node_id, recovery.attempt, option.action)
                    if (option.action == "retry") vm.action { it.actOnCircuit(next, "") }
                    else { reason = ""; decision = next }
                }) { Text(if (option.action == "retry") "Retry this step" else "I've done this — continue") }
                else option.unavailable_reason?.let { Text("${option.action}: $it", color = MeshColors.secondary) }
            }
        }
        if (run.state in setOf("pending", "running", "paused")) TextButton(enabled = !state.busy, onClick = { choose("cancel") }) { Text("Cancel run") }
    }
    val offered = decision?.takeIf { state.busy || decisionOffered(it, run) }
    if (offered != null) AlertDialog(
        onDismissRequest = { if (!state.busy) decision = null },
        title = { Text(when (offered.kind) { "approve" -> "Approve this step?"; "cancel" -> "Cancel this run?"; "recover" -> "Record completed work"; else -> checkpointLabels[offered.action] ?: offered.action }) },
        text = { Column(Modifier.verticalScroll(rememberScrollState())) {
            if (state.error.isNotEmpty()) Text(state.error, color = MeshColors.amber)
            Text("This changes Circuit Run #${run.run_id} on the desktop.")
            if (offered.kind in setOf("outcome", "recover")) OutlinedTextField(value = reason, onValueChange = { reason = it }, enabled = !state.busy,
                label = { Text(if (offered.action in setOf("review_pass", "review_fail")) "Your decision and the evidence" else "What did you do or check?") })
        } },
        confirmButton = { TextButton(enabled = !state.busy && (offered.action !in setOf("review_pass", "review_fail", "continue") || reason.isNotBlank()), onClick = {
            vm.action { api ->
                api.actOnCircuit(offered, reason)
                decision = null
                vm.notice("Circuit decision recorded")
            }
        }) { Text(if (state.busy) "Sending…" else "Confirm") } },
        dismissButton = { TextButton(enabled = !state.busy, onClick = { decision = null }) { Text("Cancel") } },
    )
}

@Composable
fun StartReviewButton(nodeId: Long, vm: BuildmeshViewModel) {
    val state by vm.state.collectAsState()
    var open by remember(nodeId) { mutableStateOf(false) }
    var options by remember(nodeId) { mutableStateOf<List<NodeReviewOption>?>(null) }
    var selected by remember(nodeId) { mutableStateOf<Long?>(null) }
    var rounds by remember(nodeId) { mutableLongStateOf(3L) }
    var readError by remember(nodeId) { mutableStateOf("") }
    LaunchedEffect(open, nodeId, state.resourceRevision) {
        if (!open) return@LaunchedEffect
        options = null
        readError = ""
        try { options = vm.api?.reviewOptions(nodeId) }
        catch (e: CancellationException) { throw e }
        catch (e: Exception) { readError = friendlyError(e); vm.report(e) }
    }
    OutlinedButton(enabled = !state.busy, onClick = { vm.dismissError(); open = true }, modifier = Modifier.fillMaxWidth()) { Text("Start review…") }
    if (open) AlertDialog(onDismissRequest = { if (!state.busy) open = false }, title = { Text("Start review") },
        text = { Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (readError.isNotEmpty()) {
                Text(readError, color = MeshColors.amber)
                TextButton(onClick = { vm.dismissError(); vm.retry() }) { Text("Retry review choices") }
            }
            else if (options == null) Text("Loading review choices…")
            else if (options!!.isEmpty()) Text("Update the desktop to start reviews from this phone.")
            else {
                Selector("Review Circuit", options!!.map { it.circuit_id to it.name }, selected, { selected = it }, enabled = !state.busy)
                if (selected == null) Selector("Review rounds", (1L..10L).map { it to "$it" }, rounds, { rounds = it }, enabled = !state.busy)
                Text("Saved Circuits keep their configured reviewer and rounds. The built-in review uses your desktop reviewer settings.")
            }
            if (readError.isEmpty() && state.error.isNotEmpty()) Text(state.error, color = MeshColors.amber)
        } },
        confirmButton = { TextButton(enabled = !state.busy && options?.any { it.circuit_id == selected } == true, onClick = {
            vm.action { api ->
                api.post("/api/nodes/$nodeId/reviews", StartNodeReviewRequest(selected, rounds).toJson())
                open = false
                vm.notice("Review started. An existing live run is kept.")
            }
        }) { Text(if (state.busy) "Starting…" else "Start review") } },
        dismissButton = { TextButton(enabled = !state.busy, onClick = { open = false }) { Text("Cancel") } })
}
