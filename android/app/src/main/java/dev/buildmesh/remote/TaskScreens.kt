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
fun NodeScreen(node: JsonObject, vm: BuildmeshViewModel, terminal: () -> Unit, changes: () -> Unit) {
    val state by vm.state.collectAsState()
    val id = node.number("id")
    var draft by rememberSaveable(id) { mutableStateOf(vm.draft("reply-$id")) }
    val lifecycle = node.objectAt("lifecycle")?.takeIf { it.text("status") == node.text("status") }
    val requestId = lifecycle?.text("timestamp").orEmpty()
    var answeredRequest by rememberSaveable(id) { mutableStateOf<String?>(null) }
    val choices = (lifecycle?.objectAt("request")?.get("choices") as? JsonArray)?.map { (it as JsonPrimitive).content }.orEmpty()
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
        Button(onClick = terminal, modifier = Modifier.fillMaxWidth()) { Text("Open terminal") }
        OutlinedButton(onClick = changes, modifier = Modifier.fillMaxWidth()) { Text("Review changes") }
        if (canReply || draft.isNotEmpty()) {
            Text("Reply to agent", style = MaterialTheme.typography.titleMedium)
            choices.forEach { choice -> Text("• $choice", color = MeshColors.secondary) }
            OutlinedTextField(enabled = !state.busy, value = draft, onValueChange = { draft = it; vm.saveDraft("reply-$id", it) }, label = { Text("Your reply") }, modifier = Modifier.fillMaxWidth(), minLines = 3, maxLines = 8)
            val fits = runCatching { inputBody(draft.trim() + "\r") }.isSuccess
            if (!fits) Text("Reply is too long. Use the terminal for longer text.", color = MeshColors.amber)
            Button(enabled = canReply && fits && draft.isNotBlank(), onClick = { send(draft.trim() + "\r") }, modifier = Modifier.fillMaxWidth()) { Text("Send reply") }
        }
    }
}

@Composable
fun TaskScreen(state: RemoteState, vm: BuildmeshViewModel, selectedMesh: Long, selectMesh: (Long) -> Unit, openNode: (Long) -> Unit) {
    var prompt by rememberSaveable { mutableStateOf(vm.draft("capture")) }
    var provider by rememberSaveable { mutableStateOf("") }
    val options = state.providers.filter { it.text("unavailable_reason").isEmpty() && (prompt.isBlank() || it.objectAt("capabilities")?.flag("supports_prefill") == true) }
    val mesh = state.meshes.firstOrNull { it.number("id") == selectedMesh }
    LaunchedEffect(state.meshes) { if (mesh == null && state.meshes.isNotEmpty()) selectMesh(state.meshes.first().number("id")) }
    LaunchedEffect(options) { if (options.none { it.text("id") == provider }) provider = options.firstOrNull()?.text("id").orEmpty() }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text("Start something", style = MaterialTheme.typography.headlineSmall)
        Text("Give an agent a task, or leave the prompt empty to open a new session.", color = MeshColors.secondary)
        Selector("Mesh", state.meshes.map { it.number("id") to it.text("name") }, selectedMesh, selectMesh, enabled = !state.busy)
        OutlinedTextField(enabled = !state.busy, value = prompt, onValueChange = { prompt = it; vm.saveDraft("capture", it) }, label = { Text("What should the agent do?") }, modifier = Modifier.fillMaxWidth(), minLines = 5, maxLines = 12)
        Selector("Launch configuration", options.map { it.text("id") to it.text("label") }, provider, { provider = it }, enabled = !state.busy)
        if (options.isEmpty()) Text("No available launch configuration supports this task. Configure an agent harness on the desktop.", color = MeshColors.amber)
        val fits = prompt.toByteArray(Charsets.UTF_8).size <= 16_000
        if (!fits) Text("The prompt must be at most 16000 bytes.", color = MeshColors.amber)
        Button(enabled = !state.busy && mesh != null && provider.isNotEmpty() && fits, onClick = {
            vm.action { api ->
                val node = api.post("/api/nodes/create", buildJsonObject {
                    put("mesh_id", selectedMesh); put("provider", provider); put("rows", 24); put("cols", 80)
                    if (prompt.isNotBlank()) put("prompt", prompt.trim())
                }).obj()
                vm.acceptNode(node)
                prompt = ""; vm.saveDraft("capture", "")
                openNode(node.number("id"))
                vm.notice("Agent started")
            }
        }, modifier = Modifier.fillMaxWidth()) { Text("Start agent") }
        Text("Your draft stays on this phone until the desktop acknowledges the new agent.", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
    }
}
