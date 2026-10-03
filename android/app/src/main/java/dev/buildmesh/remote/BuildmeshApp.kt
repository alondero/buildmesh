package dev.buildmesh.remote

import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import kotlinx.serialization.json.*

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BuildmeshApp(vm: BuildmeshViewModel, invitation: String = "") {
    val state by vm.state.collectAsStateWithLifecycle()
    var screen by rememberSaveable { mutableStateOf("home") }
    var tab by rememberSaveable { mutableStateOf("Overview") }
    var meshId by rememberSaveable { mutableLongStateOf(0) }
    var nodeId by rememberSaveable { mutableLongStateOf(0) }
    var file by rememberSaveable { mutableStateOf("") }
    LaunchedEffect(state.paired) { if (!state.paired) screen = "home" }
    fun back() { screen = when (screen) { "terminal", "changes" -> "node"; "diff" -> "changes"; else -> "home" } }
    BackHandler(state.paired && screen != "home") { back() }
    val selected = state.nodes.firstOrNull { it.number("id") == nodeId }
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.notice) {
        if (state.notice.isNotEmpty()) { snackbar.showSnackbar(state.notice); vm.dismissError() }
    }
    val title = when (screen) {
        "node", "terminal", "changes", "diff" -> selected?.text("name") ?: "Agent Node"
        "issues" -> "Issues"; "archive" -> "Archive"; "capture" -> "New task"
        "connection" -> "Connection"; "pr" -> "Create pull request"; else -> "Buildmesh"
    }
    Scaffold(
        topBar = {
            if (state.paired) TopAppBar(title = { Text(title, maxLines = 1) },
                navigationIcon = {
                    if (screen != "home") IconButton(onClick = ::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                }, actions = {
                    IconButton(onClick = vm::retry) { Icon(Icons.Default.Refresh, "Refresh") }
                    IconButton(onClick = { screen = "connection" }) { Icon(Icons.Default.Settings, "Connection settings") }
                })
        },
        bottomBar = {
            if (state.paired && screen == "home") NavigationBar {
                listOf("Overview" to Icons.Default.Dashboard, "Work" to Icons.Default.Work, "Capture" to Icons.Default.Add).forEach { (label, icon) ->
                    NavigationBarItem(selected = tab == label, onClick = {
                        if (label == "Capture") screen = "capture" else tab = label
                    }, icon = { Icon(icon, label) }, label = { Text(label) })
                }
            }
        }, snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (state.error.isNotEmpty()) ErrorBanner(state.error, vm::dismissError)
            if (state.busy || state.refreshing) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (state.restoring) Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            else if (!state.paired) PairScreen(invitation, state.busy, vm::pair)
            else DesktopStateBoundary(state.draftNamespace) {
                when (screen) {
                    "home" -> Dashboard(state, tab, meshId, { meshId = it }, { nodeId = it; screen = "node" },
                        { id, target -> meshId = id; screen = target })
                    "node" -> if (selected != null) NodeScreen(selected, vm, { screen = "terminal" }, { screen = "changes" })
                        else EmptyMessage("This node is no longer available.")
                    "terminal" -> vm.api?.let { TerminalPane(it, nodeId, vm) }
                    "changes" -> vm.api?.let { ChangesScreen(it, nodeId, vm, { file = it; screen = "diff" }) }
                    "diff" -> vm.api?.let { DiffScreen(it, nodeId, file, vm) }
                    "capture" -> TaskScreen(state, vm, meshId, { meshId = it }, { id -> nodeId = id; screen = "node" })
                    "issues", "archive" -> vm.api?.let { ResourceScreen(it, meshId, screen, state, vm, { id -> nodeId = id; screen = "node" }) }
                    "pr" -> CreatePrScreen(meshId, vm)
                    "connection" -> ConnectionScreen(state.origin, vm, {
                        meshId = 0; nodeId = 0; screen = "home"
                    })
                }
            }
        }
    }
}

@Composable
fun DesktopStateBoundary(identity: String, content: @Composable () -> Unit) {
    key(identity) { content() }
}

@Composable
fun ErrorBanner(message: String, dismiss: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.errorContainer) {
        Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(message, modifier = Modifier.weight(1f), color = MaterialTheme.colorScheme.onErrorContainer)
            IconButton(onClick = dismiss) { Icon(Icons.Default.Close, "Dismiss error") }
        }
    }
}

@Composable
fun PairScreen(invitation: String, busy: Boolean, pair: (String, String) -> Unit) {
    var url by remember { mutableStateOf(invitation) }
    var fingerprint by rememberSaveable { mutableStateOf("") }
    LaunchedEffect(invitation) { if (invitation.isNotEmpty()) url = invitation }
    val scanner = rememberLauncherForActivityResult(ScanContract()) { result -> result.contents?.let { url = it } }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(24.dp), verticalArrangement = Arrangement.spacedBy(18.dp)) {
        Spacer(Modifier.height(28.dp))
        Icon(Icons.Default.Hub, null, tint = MeshColors.accent, modifier = Modifier.size(56.dp))
        Text("Buildmesh", style = MaterialTheme.typography.headlineLarge, fontWeight = FontWeight.Bold)
        Text("Your agents, wherever you are.", color = MeshColors.secondary)
        Text("On your desktop, enable Settings → Remote Access. Connect to the same network or VPN, then scan the pairing QR code.")
        Button(enabled = !busy, onClick = {
            scanner.launch(ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE).setPrompt("Scan the Buildmesh pairing QR").setBeepEnabled(false).setOrientationLocked(false))
        }, modifier = Modifier.fillMaxWidth()) { Icon(Icons.Default.QrCodeScanner, null); Spacer(Modifier.width(8.dp)); Text("Scan pairing QR") }
        OutlinedTextField(value = url, onValueChange = { url = it }, label = { Text("Pairing URL") }, supportingText = { Text("Or paste the complete URL, including #pair=…") }, modifier = Modifier.fillMaxWidth(), maxLines = 4, enabled = !busy)
        if (!url.contains("&ca=")) OutlinedTextField(value = fingerprint, onValueChange = { fingerprint = it }, label = { Text("Root CA SHA-256 fingerprint") }, supportingText = { Text("For older desktops, copy this from Remote Access → Certificate.") }, modifier = Modifier.fillMaxWidth(), maxLines = 3, enabled = !busy)
        Button(enabled = !busy && url.isNotBlank(), onClick = { pair(url, fingerprint) }, modifier = Modifier.fillMaxWidth()) { Text(if (busy) "Pairing…" else "Pair with desktop") }
        Text("Invitations expire after five minutes and work once. Your paired device can be revoked from the desktop.", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
    }
}

@Composable
fun Dashboard(state: RemoteState, tab: String, meshId: Long, selectMesh: (Long) -> Unit, openNode: (Long) -> Unit, openMesh: (Long, String) -> Unit) {
    var query by rememberSaveable { mutableStateOf("") }
    var attentionOnly by rememberSaveable { mutableStateOf(false) }
    val visible = state.nodes.filter { node ->
        (meshId == 0L || node.number("mesh_id") == meshId) &&
            (!attentionOnly || node.text("status") == "awaiting_input") &&
            (query.isBlank() || node.text("name").contains(query, true) || node.text("branch").contains(query, true))
    }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            Text(if (tab == "Overview") "Overview" else "Your work", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
            Text("${state.nodes.count { it.text("status") == "running" }} running · ${state.nodes.count { it.text("status") == "awaiting_input" }} need attention · ${state.meshes.size} meshes", color = MeshColors.secondary)
        }
        item {
            Selector("Mesh", listOf(0L to "All meshes") + state.meshes.map { it.number("id") to it.text("name") }, meshId, selectMesh)
            OutlinedTextField(value = query, onValueChange = { query = it }, label = { Text("Search agents or branches") }, modifier = Modifier.fillMaxWidth(), singleLine = true)
            FilterChip(selected = attentionOnly, onClick = { attentionOnly = !attentionOnly }, label = { Text("Needs attention") })
        }
        if (tab == "Overview") items(state.meshes, key = { "mesh-${it.number("id")}" }) { mesh ->
            Card(colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                Column(Modifier.fillMaxWidth().padding(14.dp)) {
                    Text(mesh.text("name"), style = MaterialTheme.typography.titleMedium)
                    Text(mesh.text("path"), style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        TextButton(onClick = { openMesh(mesh.number("id"), "issues") }) { Text("Issues") }
                        TextButton(onClick = { openMesh(mesh.number("id"), "archive") }) { Text("Archive") }
                        TextButton(onClick = { openMesh(mesh.number("id"), "pr") }) { Text("Create PR") }
                    }
                }
            }
        }
        if (visible.isEmpty()) item { EmptyMessage(if (state.refreshing) "Loading agents…" else "No agents match this view. Capture a task to start one.") }
        items(visible.sortedByDescending { it.text("status") == "awaiting_input" }, key = { "node-${it.number("id")}" }) { node ->
            Card(onClick = { openNode(node.number("id")) }, colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(node.text("name"), style = MaterialTheme.typography.titleMedium)
                    Text(nodeLabel(node), color = MeshColors.status(node.text("status")))
                    Text("${state.meshes.firstOrNull { it.number("id") == node.number("mesh_id") }?.text("name").orEmpty()} · ${node.text("branch")}", color = MeshColors.secondary)
                    node.objectAt("lifecycle")?.text("message")?.takeIf { it.isNotBlank() }?.let { Text(it, maxLines = 3) }
                }
            }
        }
    }
}

@Composable
fun <T> Selector(label: String, choices: List<Pair<T, String>>, value: T, onChange: (T) -> Unit, enabled: Boolean = true) {
    var expanded by remember { mutableStateOf(false) }
    Box(Modifier.fillMaxWidth()) {
        OutlinedButton(enabled = enabled, onClick = { expanded = true }, modifier = Modifier.fillMaxWidth()) {
            Text("$label: ${choices.firstOrNull { it.first == value }?.second ?: "Choose"}", modifier = Modifier.weight(1f))
            Icon(Icons.Default.ExpandMore, null)
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            choices.forEach { (id, name) -> DropdownMenuItem(enabled = enabled, text = { Text(name) }, onClick = { expanded = false; onChange(id) }) }
        }
    }
}

@Composable
fun EmptyMessage(message: String) {
    Text(message, modifier = Modifier.padding(24.dp), color = MeshColors.secondary)
}

@Composable
fun ConnectionScreen(origin: String, vm: BuildmeshViewModel, onForgot: () -> Unit) {
    var confirm by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text("Paired desktop", style = MaterialTheme.typography.titleLarge)
        Text(origin, color = MeshColors.secondary)
        Text("Your device session is encrypted on this phone. To revoke access, remove this device in the desktop's Authorized Devices settings.")
        Button(onClick = vm::refresh) { Text("Retry connection") }
        OutlinedButton(onClick = { confirm = true }) { Text("Forget this desktop") }
    }
    if (confirm) AlertDialog(onDismissRequest = { confirm = false }, title = { Text("Forget this desktop?") }, text = { Text("You'll need a fresh invitation to connect again. Revoke this device on the desktop to remove its authorization there too.") },
        confirmButton = { TextButton(onClick = { vm.forget(); onForgot() }) { Text("Forget") } }, dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } })
}
