package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.*

@Composable
fun NodeScreen(node: JsonObject, vm: BuildmeshViewModel, terminal: () -> Unit, changes: () -> Unit, createPr: () -> Unit) {
    val state by vm.state.collectAsState()
    val id = node.number("id")
    var draft by rememberSaveable(state.draftNamespace, id) { mutableStateOf(vm.draft("reply-$id")) }
    val lifecycle = node.objectAt("lifecycle")?.takeIf { it.text("status") == node.text("status") }
    val requestId = lifecycle?.text("timestamp").orEmpty()
    var answeredRequest by rememberSaveable(state.draftNamespace, id) { mutableStateOf<String?>(null) }
    val choices = (lifecycle?.objectAt("request")?.get("choices") as? JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull }.orEmpty()
    val canReply = node.text("status") in setOf("idle", "ready", "awaiting_input") && !state.busy && (requestId.isEmpty() || answeredRequest != requestId)
    fun send(sequence: String) {
        vm.action { api ->
            api.sendKeys(id, sequence)
            answeredRequest = requestId.takeIf { it.isNotEmpty() }
            draft = ""; vm.saveDraft("reply-$id", "")
            vm.notice("Reply delivered")
        }
    }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text(node.text("name"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
        Text(nodeLabel(node), color = MeshColors.status(node.text("status")), style = MaterialTheme.typography.titleMedium)
        Text("${node.text("provider")} · ${node.text("branch")}", color = MeshColors.secondary)
        if (node.text("signal_health") in setOf("unavailable", "degraded")) Text("Session observation: ${node.text("signal_health")}. Check the terminal for current activity.", color = MeshColors.amber)
        if (node.text("signal_health") == "unverified") Text("Session observation has not been verified yet.", color = MeshColors.secondary)
        lifecycle?.text("message")?.takeIf { it.isNotBlank() }?.let { Text(it) }
        lifecycle?.objectAt("semantic_turn")?.text("description")?.takeIf { it.isNotBlank() && it != lifecycle.text("message") }?.let { Text(it) }
        Button(shape = MaterialTheme.shapes.small, onClick = terminal, modifier = Modifier.fillMaxWidth()) { Text("Open terminal") }
        OutlinedButton(shape = MaterialTheme.shapes.small, onClick = changes, modifier = Modifier.fillMaxWidth()) { Text("Review changes") }
        OutlinedButton(shape = MaterialTheme.shapes.small, onClick = createPr, modifier = Modifier.fillMaxWidth()) { Text("Create pull request") }
        vm.api?.let { PullRequestsPanel(it, node.number("mesh_id"), vm, id) }
        if (canReply || draft.isNotEmpty()) {
            Text("Reply to agent", style = MaterialTheme.typography.titleMedium)
            choices.forEach { choice -> Text("• $choice", color = MeshColors.secondary) }
            OutlinedTextField(colors = buildmeshTextFieldColors(), enabled = !state.busy, value = draft, onValueChange = { draft = it; vm.saveDraft("reply-$id", it) }, label = { Text("Your reply") }, modifier = Modifier.fillMaxWidth(), minLines = 3, maxLines = 8)
            val fits = runCatching { inputBody(draft.trim() + "\r") }.isSuccess
            if (!fits) Text("Reply is too long. Use the terminal for longer text.", color = MeshColors.amber)
            Button(shape = MaterialTheme.shapes.small, enabled = canReply && fits && draft.isNotBlank(), onClick = { send(draft.trim() + "\r") }, modifier = Modifier.fillMaxWidth()) { Text("Send reply") }
        }
    }
}

@Composable
// `launch` is a menu row id, or "" for the mesh default; the Overview's spawn
// button pre-selects it before opening this screen.
fun TaskScreen(state: RemoteState, vm: BuildmeshViewModel, selectedMesh: Long, selectMesh: (Long) -> Unit,
    launch: String, selectLaunch: (String) -> Unit, openNode: (Long) -> Unit) {
    var prompt by rememberSaveable(state.draftNamespace) { mutableStateOf(vm.draft("capture")) }
    // Issue #2331 — one operation key per *submission*, reused for every retry
    // of it, so a retry after a lost response resolves to the same node instead
    // of creating a duplicate. Editing the prompt, the mesh, or the launch
    // configuration starts a new submission, which must mint a fresh key: the
    // desktop rejects a key reused with different content as a conflict.
    var operationKey by rememberSaveable(state.draftNamespace) { mutableStateOf("") }
    var operationFor by rememberSaveable(state.draftNamespace) { mutableStateOf("") }
    val submission = "$selectedMesh|$launch|$prompt"
    LaunchedEffect(submission) {
        if (operationFor != submission) {
            operationKey = ""
            operationFor = submission
        }
    }
    val options = launchOptions(state.providers, needsPrompt = false)
    val chosen = options.firstOrNull { it.text("id") == launch }
    val mesh = state.meshes.firstOrNull { it.number("id") == selectedMesh }
    val defaultName = meshDefaultName(mesh, state.providers)
    LaunchedEffect(state.meshes) { if (mesh == null && state.meshes.isNotEmpty()) selectMesh(state.meshes.first().number("id")) }
    // A row that left the menu (harness removed, now unavailable) falls back
    // to the mesh default rather than launching something else.
    LaunchedEffect(options, launch) { if (launch.isNotEmpty() && chosen == null && state.providers.isNotEmpty()) selectLaunch("") }
    // An explicit choice is kept even when it cannot take the prompt, so the
    // user sees why instead of the selection silently changing under them.
    val promptRejected = prompt.isNotBlank() && chosen != null && !supportsPrompt(chosen)
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text("Start something", style = MaterialTheme.typography.headlineSmall)
        Text("Give an agent a task, or leave the prompt empty to open a new session.", color = MeshColors.secondary)
        Selector("Mesh", state.meshes.map { it.number("id") to it.text("name") }, selectedMesh, selectMesh, enabled = !state.busy)
        OutlinedTextField(colors = buildmeshTextFieldColors(), enabled = !state.busy, value = prompt, onValueChange = { prompt = it; vm.saveDraft("capture", it) }, label = { Text("What should the agent do?") }, modifier = Modifier.fillMaxWidth(), minLines = 5, maxLines = 12)
        Selector("Launch configuration", listOf("" to (defaultName?.let { "Mesh default ($it)" } ?: "Mesh default")) + options.map { it.text("id") to it.text("label") }, launch, selectLaunch, enabled = !state.busy)
        if (options.isEmpty()) Text("No launch configuration is available. Configure an agent harness on the desktop.", color = MeshColors.amber)
        if (promptRejected) Text("${chosen?.text("label")} can't take an initial prompt. Clear the prompt to open a plain session, or choose another launch configuration.", color = MeshColors.amber)
        val fits = prompt.toByteArray(Charsets.UTF_8).size <= 16_000
        if (!fits) Text("The prompt must be at most 16000 bytes.", color = MeshColors.amber)
        Button(shape = MaterialTheme.shapes.small, enabled = !state.busy && mesh != null && options.isNotEmpty() && !promptRejected && fits, onClick = {
            val key = operationKey.ifEmpty { newOperationKey() }
            operationKey = key
            vm.action { api ->
                val outcome = readCreateNodeOutcome(api.post("/api/nodes/create", createNodeBody(selectedMesh, chosen, prompt, key)).obj())
                when (outcome) {
                    // Running: the draft is done and the new node is the focus.
                    is CreateNodeOutcome.Active -> {
                        vm.acceptNode(outcome.node)
                        operationKey = ""; prompt = ""; vm.saveDraft("capture", "")
                        openNode(outcome.node.number("id"))
                        vm.notice("Agent started")
                    }
                    // Committed but not running (#2331): the node exists, so
                    // send the user to it instead of showing the same message
                    // as "nothing happened", which invited a duplicate retry.
                    is CreateNodeOutcome.StartPending -> {
                        vm.acceptNode(outcome.node)
                        operationKey = ""; prompt = ""; vm.saveDraft("capture", "")
                        openNode(outcome.node.number("id"))
                        vm.notice("Node created but not started: ${outcome.reason}")
                    }
                    is CreateNodeOutcome.StartFailed -> {
                        vm.acceptNode(outcome.node)
                        operationKey = ""; prompt = ""; vm.saveDraft("capture", "")
                        openNode(outcome.node.number("id"))
                        vm.notice("Node created but not started: ${outcome.startError}")
                    }
                }
            }
        }, modifier = Modifier.fillMaxWidth()) { Text("Start agent") }
        Text("Your draft stays on this phone until the desktop acknowledges the new agent.", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
    }
}
