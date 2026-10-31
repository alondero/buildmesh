package dev.buildmesh.remote

import android.Manifest
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import kotlinx.serialization.json.*

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BuildmeshApp(vm: BuildmeshViewModel, invitation: String = "", requestedNodeId: Long = 0L, onNodeRequestConsumed: () -> Unit = {}) {
    val state by vm.state.collectAsStateWithLifecycle()
    var screen by rememberSaveable { mutableStateOf("home") }
    var tab by rememberSaveable { mutableStateOf("Overview") }
    var meshId by rememberSaveable { mutableLongStateOf(0) }
    var nodeId by rememberSaveable { mutableLongStateOf(0) }
    var launch by rememberSaveable { mutableStateOf("") }
    var file by rememberSaveable { mutableStateOf("") }
    var pendingNodeId by rememberSaveable { mutableLongStateOf(0L) }
    LaunchedEffect(state.paired) { if (!state.paired) screen = "home" }
    // A notification names the node it is about, so opening it lands on that
    // node. The request waits for the inventory before it navigates: an empty
    // node list before the first read means "not loaded yet", not "that node
    // is gone". The request is consumed so the same tap opens it again later.
    LaunchedEffect(requestedNodeId) {
        if (requestedNodeId <= 0L) return@LaunchedEffect
        pendingNodeId = requestedNodeId
        onNodeRequestConsumed()
    }
    LaunchedEffect(pendingNodeId, state.nodes, state.loaded, state.refreshing) {
        if (pendingNodeId <= 0L) return@LaunchedEffect
        val present = state.nodes.any { it.number("id") == pendingNodeId }
        if (present || (state.loaded && !state.refreshing)) {
            nodeId = pendingNodeId
            screen = "node"
            pendingNodeId = 0L
        }
    }
    fun back() { screen = when (screen) { "terminal", "changes", "pr" -> "node"; "diff" -> "changes"; else -> "home" } }
    BackHandler(state.paired && screen != "home") { back() }
    val selected = state.nodes.firstOrNull { it.number("id") == nodeId }
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.notice) {
        if (state.notice.isNotEmpty()) { snackbar.showSnackbar(state.notice); vm.dismissError() }
    }
    // The five-second poll is not an event the user asked for, so it must not
    // drive the chrome: the bar belongs to the first load and to a read the
    // user requested (`vm.retry()`), not to every idle snapshot.
    val showProgress = state.busy || state.manualRefresh || (state.refreshing && !state.loaded)
    var pulling by remember { mutableStateOf(false) }
    LaunchedEffect(state.refreshing) { if (pulling && !state.refreshing) pulling = false }
    Scaffold(
        topBar = {
            if (state.paired) TopAppBar(title = {
                if (screen == "home") BuildmeshWordmark() else {
                    val title = when (screen) {
                        "node", "terminal", "changes", "diff", "pr" -> selected?.text("name") ?: "Agent Node"
                        "issues" -> "Issues"; "pulls" -> "Pull requests"; "archive" -> "Archive"; "capture" -> "New task"
                        "connection" -> "Connection"; "usage" -> "Usage"; else -> "Buildmesh"
                    }
                    Text(title, maxLines = 1)
                }
            }, colors = TopAppBarDefaults.topAppBarColors(
                containerColor = MeshColors.surface, scrolledContainerColor = MeshColors.surface,
                navigationIconContentColor = MeshColors.secondary, actionIconContentColor = MeshColors.secondary,
            ),
                navigationIcon = {
                    if (screen != "home") IconButton(onClick = ::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                }, actions = {
                    if (screen == "home") IconButton(onClick = { screen = "usage" }) { Icon(Icons.Default.BarChart, "Usage") }
                    // The shared recovery affordance every screen offers: it
                    // re-reads the screen's own route and keeps the thin
                    // progress bar, so a pull never needs a second control.
                    IconButton(onClick = vm::retry) { Icon(Icons.Default.Refresh, "Refresh") }
                    IconButton(onClick = { screen = "connection" }) { Icon(Icons.Default.Settings, "Connection settings") }
                })
        },
        bottomBar = {
            if (state.paired && screen == "home") NavigationBar(containerColor = MeshColors.surface, tonalElevation = 0.dp) {
                listOf("Overview" to Icons.Default.Dashboard, "Work" to Icons.Default.Work, "Capture" to Icons.Default.Add).forEach { (label, icon) ->
                    NavigationBarItem(selected = tab == label, colors = NavigationBarItemDefaults.colors(
                        selectedIconColor = MeshColors.accent, selectedTextColor = MeshColors.accent,
                        indicatorColor = MeshColors.selection,
                        unselectedIconColor = MeshColors.secondary, unselectedTextColor = MeshColors.secondary,
                    ), onClick = {
                        if (label == "Capture") screen = "capture" else tab = label
                    }, icon = { Icon(icon, label) }, label = { Text(label) })
                }
            }
        }, snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (state.error.isNotEmpty()) ErrorBanner(state.error, vm::dismissError)
            if (showProgress) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (state.restoring) Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            else if (!state.paired) PairScreen(invitation, state.busy, vm::pair)
            else DesktopStateBoundary(state.draftNamespace) {
                when (screen) {
                    // Pull-to-refresh is the phone's own gesture for a list the
                    // user is staring at; the app-bar action stays as the
                    // recovery affordance the error notice points at.
                    "home" -> PullToRefreshBox(
                        isRefreshing = pulling,
                        // The gesture owns its own indicator, so this read is not
                        // marked manual: only a started read spins, and only
                        // until it lands.
                        onRefresh = { if (vm.refresh()) pulling = true },
                        modifier = Modifier.fillMaxSize(),
                    ) {
                        Dashboard(state, tab, meshId, { meshId = it }, { nodeId = it; screen = "node" },
                            { id, target ->
                                meshId = id
                                if (target == "work") tab = "Work" else screen = target
                            }, { id, option -> meshId = id; launch = option; screen = "capture" },
                            closeNode = vm::closeNode,
                            openTerminal = { id -> nodeId = id; screen = "terminal" },
                            // `onDone(true)` is the desktop accepting the keys;
                            // `onDone(false)` is the send failing, which releases
                            // the chip's in-flight marker.
                            answer = { id, sequence, onDone ->
                                vm.action(onFailed = { onDone(false) }) { api -> api.sendKeys(id, sequence); onDone(true) }
                            })
                    }
                    "node" -> if (selected != null) NodeScreen(selected, vm, { screen = "terminal" }, { screen = "changes" }, { screen = "pr" }, { id -> nodeId = id; screen = "terminal" })
                        else EmptyMessage("This node is no longer available.")
                    "terminal" -> vm.api?.let { TerminalPane(it, nodeId, vm) }
                    "changes" -> vm.api?.let { ChangesScreen(it, nodeId, vm, { file = it; screen = "diff" }) }
                    "diff" -> vm.api?.let { DiffScreen(it, nodeId, file, vm) }
                    "capture" -> TaskScreen(state, vm, meshId, { meshId = it }, launch, { launch = it }, { id -> nodeId = id; screen = "node" })
                    "issues", "archive" -> vm.api?.let { ResourceScreen(it, meshId, screen, state, vm, { id -> nodeId = id; screen = "node" }) }
                    "pulls" -> vm.api?.let { PullRequestsScreen(it, meshId, vm, { id -> nodeId = id; screen = "node" }) }
                    "usage" -> vm.api?.let { UsageScreen(it, vm) }
                    "pr" -> if (selected != null) vm.api?.let { CreatePrScreen(it, selected.number("mesh_id"), nodeId, vm) }
                        else EmptyMessage("This node is no longer available.")
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
    // A scanned QR is not redelivered after a rotation: MainActivity clears the
    // intent it arrived on, so `invitation` reads empty in the recreated
    // activity. Save the field or rotating wipes the pairing URL.
    var url by rememberSaveable { mutableStateOf(invitation) }
    var fingerprint by rememberSaveable { mutableStateOf("") }
    LaunchedEffect(invitation) { if (invitation.isNotEmpty()) url = invitation }
    val scanner = rememberLauncherForActivityResult(ScanContract()) { result -> result.contents?.let { url = it } }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(24.dp), verticalArrangement = Arrangement.spacedBy(18.dp)) {
        Spacer(Modifier.height(28.dp))
        BuildmeshWordmark(markSize = 64.dp)
        Text("Your agents, wherever you are.", color = MeshColors.secondary)
        Text("On your desktop, enable Settings → Remote Access. Connect to the same network or VPN, then scan the pairing QR code.")
        Button(shape = MaterialTheme.shapes.small, enabled = !busy, onClick = {
            scanner.launch(ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE).setPrompt("Scan the Buildmesh pairing QR").setBeepEnabled(false).setOrientationLocked(false))
        }, modifier = Modifier.fillMaxWidth()) { Icon(Icons.Default.QrCodeScanner, null); Spacer(Modifier.width(8.dp)); Text("Scan pairing QR") }
        OutlinedTextField(colors = buildmeshTextFieldColors(), value = url, onValueChange = { url = it }, label = { Text("Pairing URL") }, supportingText = { Text("Or paste the complete URL, including #pair=…") }, modifier = Modifier.fillMaxWidth(), maxLines = 4, enabled = !busy)
        if (!url.contains("&ca=")) OutlinedTextField(colors = buildmeshTextFieldColors(), value = fingerprint, onValueChange = { fingerprint = it }, label = { Text("Root CA SHA-256 fingerprint") }, supportingText = { Text("For older desktops, copy this from Remote Access → Certificate.") }, modifier = Modifier.fillMaxWidth(), maxLines = 3, enabled = !busy)
        Button(shape = MaterialTheme.shapes.small, enabled = !busy && url.isNotBlank(), onClick = { pair(url, fingerprint) }, modifier = Modifier.fillMaxWidth()) { Text(if (busy) "Pairing…" else "Pair with desktop") }
        Text("Invitations expire after five minutes and work once. Your paired device can be revoked from the desktop.", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
    }
}

private val MeshSplitMinWidth = 600.dp

@Composable
fun Dashboard(state: RemoteState, tab: String, meshId: Long, selectMesh: (Long) -> Unit, openNode: (Long) -> Unit, openMesh: (Long, String) -> Unit,
    openCapture: (Long, String) -> Unit,
    closeNode: ((Long) -> Unit)? = null,
    openTerminal: (Long) -> Unit = {},
    // `answer` returns false when the reply never reached the transport, and
    // completes with `true` only once the desktop accepted it — or `false`
    // when the send failed, so a tapped chip retires only on an acceptance
    // and re-enables itself on a failure (issue #1966 review).
    answer: (nodeId: Long, sequence: String, onDone: (accepted: Boolean) -> Unit) -> Boolean = { _, _, _ -> false }) {
    var closingId by rememberSaveable { mutableStateOf<Long?>(null) }
    val closing = state.nodes.firstOrNull { it.number("id") == closingId && it.text("status") != "archived" }
    if (closing != null && closeNode != null) AlertDialog(
        onDismissRequest = { closingId = null },
        title = { Text("Close ${closing.text("name")}?") },
        text = { Text("Stop this agent and remove it from Work? Its worktree and files will be kept.") },
        confirmButton = { TextButton(enabled = !state.busy, onClick = {
            closingId = null
            closeNode(closing.number("id"))
        }) { Text("Close agent", color = MaterialTheme.colorScheme.error) } },
        dismissButton = { TextButton(onClick = { closingId = null }) { Text("Cancel") } },
    )
    val openNodes = state.nodes.filter { it.text("status") != "archived" }
    val nodesByMesh = openNodes.groupBy { it.number("mesh_id") }
    // Match Sidebar.tsx: open nodes determine the band; both bands keep saved mesh order.
    val (activeMeshes, inactiveMeshes) = state.meshes.partition { nodesByMesh.containsKey(it.number("id")) }
    val meshes = activeMeshes + inactiveMeshes
    BoxWithConstraints(Modifier.fillMaxSize()) {
        // A phone keeps the two steps apart. A tablet, an unfolded fold, or any
        // window at least 600dp wide keeps the mesh list beside the agents.
        val wide = maxWidth >= MeshSplitMinWidth
        if (wide) {
            Row(Modifier.fillMaxSize()) {
                MeshDirectory(meshes, nodesByMesh, state, meshId, selectMesh, openMesh, openCapture, "Meshes",
                    Modifier.width(280.dp).fillMaxHeight())
                Box(Modifier.width(1.dp).fillMaxHeight().background(MeshColors.borderStrong))
                MeshWork(state, meshId, openNode, openMesh, openTerminal, answer, closeNode, { closingId = it },
                    Modifier.weight(1f).fillMaxHeight())
            }
        } else if (tab == "Overview") {
            MeshDirectory(meshes, nodesByMesh, state, meshId, selectMesh, openMesh, openCapture, "Overview", Modifier.fillMaxSize())
        } else {
            MeshWork(state, meshId, openNode, openMesh, openTerminal, answer, closeNode, { closingId = it }, Modifier.fillMaxSize())
        }
    }
}

@Composable
private fun MeshDirectory(
    meshes: List<JsonObject>,
    nodesByMesh: Map<Long, List<JsonObject>>,
    state: RemoteState,
    selectedMeshId: Long,
    selectMesh: (Long) -> Unit,
    openMesh: (Long, String) -> Unit,
    openCapture: (Long, String) -> Unit,
    heading: String,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier, contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            Text(heading, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
        }
        items(meshes, key = { "mesh-${it.number("id")}" }) { mesh ->
            val members = nodesByMesh[mesh.number("id")].orEmpty()
            val id = mesh.number("id")
            Card(colors = CardDefaults.cardColors(containerColor = if (id == selectedMeshId) MeshColors.selection else MeshColors.card)) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Column(Modifier.weight(1f).clickable(role = Role.Button, onClickLabel = "View work in ${mesh.text("name")}") {
                        selectMesh(id)
                        openMesh(id, "work")
                    }.heightIn(min = 48.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(mesh.text("name"), style = MaterialTheme.typography.titleMedium,
                            color = if (members.isEmpty()) MeshColors.muted else MeshColors.text,
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                        MeshActivitySummary(members)
                    }
                    // Both halves open Capture pre-filled, where the first
                    // prompt is written; an empty prompt opens a plain session.
                    SpawnSplitButton(meshDefaultName(mesh, state.providers), launchOptions(state.providers, needsPrompt = false),
                        "a task in ${mesh.text("name")}", true,
                        { openCapture(id, "") }, { openCapture(id, it.text("id")) }, compact = true)
                }
            }
        }
        if (meshes.isEmpty()) item {
            EmptyMessage(if (state.refreshing) "Loading agents…" else "No meshes yet. Add a repository on the desktop to start.")
        }
    }
}

@Composable
private fun MeshWork(
    state: RemoteState,
    meshId: Long,
    openNode: (Long) -> Unit,
    openMesh: (Long, String) -> Unit,
    openTerminal: (Long) -> Unit,
    answer: (nodeId: Long, sequence: String, onDone: (accepted: Boolean) -> Unit) -> Boolean,
    closeNode: ((Long) -> Unit)?,
    onRequestClose: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    val mesh = state.meshes.firstOrNull { it.number("id") == meshId }
    val scope = if (mesh == null) emptyList() else state.nodes.filter {
        it.text("status") != "archived" && it.number("mesh_id") == meshId
    }
    val grouped = if (mesh == null) emptyList() else matchingWorkActivities(state, meshId, "", false)
    // A pair stays one bracket. A waiting agent who is on their own is pinned
    // in the deck and left out of the rows, so that agent is not listed twice.
    val pairedIds = grouped.filter { it.members.size > 1 }.flatMap { it.members }.map { it.number("id") }.toSet()
    val deckScope = scope.filter { it.number("id") !in pairedIds }
    val waitingIds = waitingForInput(deckScope).map { it.number("id") }.toSet()
    val activities = grouped.mapNotNull { activity ->
        if (activity.members.size > 1) activity
        else if (activity.members.any { it.number("id") in waitingIds }) null
        else activity
    }
    val glyphMotion = rememberWorkGlyphMotion()
    LazyColumn(modifier, contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        if (mesh == null) {
            item { EmptyMessage("Select a mesh to see its agents.") }
        } else {
            item {
                Text(mesh.text("name"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            item {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = { openMesh(meshId, "pulls") }) { Text("Pull requests") }
                    TextButton(onClick = { openMesh(meshId, "issues") }) { Text("Issues") }
                    TextButton(onClick = { openMesh(meshId, "archive") }) { Text("Archive") }
                }
            }
            if (waitingIds.isNotEmpty()) item(key = "waiting-for-input") {
                AttentionDeck(deckScope, state.meshes, state.busy, openNode, openTerminal, answer)
            }
            items(activities, key = { "activity-${it.root.number("id")}" }) {
                WorkActivityRows(it, state, glyphMotion, openNode, closeNode?.let { { id -> onRequestClose(id) } })
            }
            if (activities.isEmpty() && waitingIds.isEmpty()) item {
                EmptyMessage(if (state.refreshing) "Loading agents…" else "No agents in this mesh yet.")
            }
        }
    }
}

@Composable
private fun MeshActivitySummary(nodes: List<JsonObject>) {
    val count = nodes.size
    val summary = if (count == 0) "No agents yet" else
        "$count agent${if (count == 1) "" else "s"}: ${nodes.joinToString(", ") { nodeLabel(it) }}"
    Row(Modifier.fillMaxWidth().semantics(mergeDescendants = true) { contentDescription = summary },
        horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
        if (count == 0) Text("No agents yet", style = MaterialTheme.typography.bodySmall, color = MeshColors.muted)
        else {
            FlowRow(Modifier.weight(1f, fill = false), horizontalArrangement = Arrangement.spacedBy(3.dp),
                verticalArrangement = Arrangement.spacedBy(3.dp)) {
                nodes.forEach { node ->
                    Box(Modifier.size(6.dp).background(MeshColors.status(node.text("status")), CircleShape))
                }
            }
            Text("$count", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
        }
    }
}

@Composable
fun <T> Selector(label: String, choices: List<Pair<T, String>>, value: T, onChange: (T) -> Unit, enabled: Boolean = true) {
    var expanded by remember { mutableStateOf(false) }
    Box(Modifier.fillMaxWidth()) {
        OutlinedButton(shape = MaterialTheme.shapes.small, enabled = enabled, onClick = { expanded = true }, modifier = Modifier.fillMaxWidth()) {
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
    val context = LocalContext.current
    var watching by remember { mutableStateOf(AgentWatchService.isEnabled(context)) }
    var refused by remember { mutableStateOf(false) }
    val askForNotices = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            AgentWatchService.setEnabled(context, true)
            watching = true
            refused = false
        } else {
            refused = true
        }
    }
    fun watch(enabled: Boolean) {
        if (!enabled) {
            AgentWatchService.setEnabled(context, false)
            watching = false
            refused = false
            return
        }
        if (!AgentNotifications.allowed(context)) {
            askForNotices.launch(Manifest.permission.POST_NOTIFICATIONS)
            return
        }
        AgentWatchService.setEnabled(context, true)
        watching = true
        refused = false
    }
    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text("Paired desktop", style = MaterialTheme.typography.titleLarge)
        Text(origin, color = MeshColors.secondary)
        Text("Your device session is encrypted on this phone. To revoke access, remove this device in the desktop's Authorized Devices settings.")
        Button(shape = MaterialTheme.shapes.small, onClick = vm::refresh) { Text("Retry connection") }
        OutlinedButton(shape = MaterialTheme.shapes.small, onClick = { confirm = true }) { Text("Forget this desktop") }
        HorizontalDivider()
        Text("Background notifications", style = MaterialTheme.typography.titleMedium)
        Text(
            "Keep this phone listening to the desktop while the app is closed, so an agent that asks a question or requests permission reaches you. Turning it off stops the background connection.",
            color = MeshColors.secondary,
        )
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(if (watching) "On" else "Off", modifier = Modifier.weight(1f))
            Switch(checked = watching, onCheckedChange = { watch(it) })
        }
        if (refused) {
            Text(
                "Buildmesh needs permission to post notifications before it can tell you about an agent.",
                color = MeshColors.secondary,
            )
        }
    }
    if (confirm) AlertDialog(onDismissRequest = { confirm = false }, title = { Text("Forget this desktop?") }, text = { Text("You'll need a fresh invitation to connect again. Revoke this device on the desktop to remove its authorization there too.") },
        confirmButton = { TextButton(onClick = { vm.forget(); onForgot() }) { Text("Forget") } }, dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } })
}
