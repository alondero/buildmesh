package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl

// `decode` runs inside the guard so a response shape this build does not
// understand becomes an error notice instead of a throw during composition.
//
// `onRead` fires when a read lands, so a pull that asked for it can retire
// its own indicator — including when the answer is identical to the last one.
@Composable
internal fun <T> rememberRemote(api: BuildmeshApi, path: String, vm: BuildmeshViewModel, reload: Int = 0, onRead: (() -> Unit)? = null, decode: (JsonElement) -> T): State<RemoteLoad<T>> {
    val state by vm.state.collectAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    return produceState(RemoteLoad(), api, path, reload, state.resourceRevision, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            value = RemoteLoad()
            try { value = RemoteLoad(loading = false, data = decode(api.get(path))) }
            catch (e: CancellationException) { throw e }
            catch (e: Exception) { value = RemoteLoad(loading = false, error = friendlyError(e)); vm.report(e) }
            onRead?.invoke()
            awaitCancellation()
        }
    }
}

internal data class RemoteLoad<T>(val loading: Boolean = true, val data: T? = null, val error: String = "")

@Composable
internal fun LoadNotice(load: RemoteLoad<*>, retry: () -> Unit) {
    if (load.loading) EmptyMessage("Loading…")
    else if (load.error.isNotEmpty()) Column {
        EmptyMessage(load.error)
        TextButton(onClick = retry) { Text("Retry") }
    }
}

@Composable
fun ResourceScreen(api: BuildmeshApi, meshId: Long, screen: String, state: RemoteState, vm: BuildmeshViewModel, openNode: (Long) -> Unit) {
    if (screen == "issues") {
        IssuesResourceScreen(api, meshId, state, vm, openNode)
        return
    }
    ArchiveResourceScreen(api, meshId, state, vm, openNode)
}

/**
 * The mesh's archived sessions.
 *
 * The list refreshes by pulling down, and the app-bar action stays the
 * shared recovery affordance; a failed read keeps its own Retry, above the
 * fold, so the screen never needs a scroll to recover.
 */
@Composable
private fun ArchiveResourceScreen(api: BuildmeshApi, meshId: Long, state: RemoteState, vm: BuildmeshViewModel, openNode: (Long) -> Unit) {
    val refresh = remember { ListingRefresh() }
    val load by rememberRemote(api, "/api/meshes/$meshId/agent-nodes/discover", vm, refresh.reload, onRead = { refresh.settle() }) { it.objects() }
    val resources = load.data
    var selected by remember(state.draftNamespace, meshId) { mutableStateOf<JsonObject?>(null) }
    var provider by rememberSaveable(state.draftNamespace, meshId) { mutableStateOf("") }
    val options = state.providers.filter {
        it.text("unavailable_reason").isEmpty() && launchableWithoutConfiguration(it) && it.flag("resumable")
    }
    LaunchedEffect(options) { if (options.none { it.text("id") == provider }) provider = options.firstOrNull()?.text("id").orEmpty() }
    PullToRefreshList(refresh, Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item { LoadNotice(load) { refresh.request() } }
            if (resources != null && resources.isEmpty()) item { EmptyMessage("No archived sessions found.") }
            items(resources.orEmpty()) { item ->
                Card(colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(item.text("first_message").ifEmpty { "Archived session" }, style = MaterialTheme.typography.titleMedium)
                        Text("${item.text("branch")} · ${item.text("timestamp")}", color = MeshColors.secondary)
                        Button(shape = MaterialTheme.shapes.small, enabled = !state.busy && options.isNotEmpty(), onClick = { selected = item }) { Text("Import and resume") }
                    }
                }
            }
        }
    }
    selected?.let { item ->
        AlertDialog(onDismissRequest = { if (!state.busy) selected = null }, title = { Text("Choose a launch configuration") },
            text = { Selector("Launch configuration", options.map { it.text("id") to it.text("label") }, provider, { provider = it }, enabled = !state.busy) },
            confirmButton = { TextButton(enabled = !state.busy && provider.isNotEmpty(), onClick = {
                vm.action { client ->
                    val node = try { client.post("/api/meshes/$meshId/agent-nodes/import-and-resume", buildJsonObject {
                        put("cli_session_id", item.text("session_id")); put("branch", item.text("branch").ifEmpty { "main" }); put("provider", provider)
                        item.text("worktree_name").takeIf { it.isNotEmpty() }?.let { put("worktree_name", it) }
                    }) } catch (e: ApiException) {
                        if (e.status == 207) selected = null
                        throw e
                    }
                    vm.acceptNode(node.obj())
                    selected = null; openNode(node.obj().number("id"))
                }
            }) { Text(if (state.busy) "Starting…" else "Start") } },
            dismissButton = { TextButton(enabled = !state.busy, onClick = { selected = null }) { Text("Cancel") } })
    }
}

@Composable
fun ChangesScreen(api: BuildmeshApi, id: Long, vm: BuildmeshViewModel, openFile: (String) -> Unit) {
    val refresh = remember { ListingRefresh() }
    val changesLoad by rememberRemote(api, "/api/agents/$id/git/status", vm, refresh.reload, onRead = { refresh.settle() }) { it.objects() }
    val branchLoad by rememberRemote(api, "/api/agents/$id/git/branch", vm, refresh.reload, onRead = { refresh.settle() }) { it.obj().text("branch") }
    val changes = changesLoad.data
    PullToRefreshList(refresh, Modifier.fillMaxSize(), reads = 2) {
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item { Text("Branch: ${branchLoad.data.orEmpty()}", color = MeshColors.secondary) }
            item { LoadNotice(changesLoad) { refresh.request() }; LoadNotice(branchLoad) { refresh.request() } }
            if (changes != null && changes.isEmpty()) item { EmptyMessage("Working tree is clean.") }
            items(changes.orEmpty()) { entry ->
                Card(onClick = { openFile(entry.text("path")) }, colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                    Column(Modifier.fillMaxWidth().padding(16.dp)) {
                        Text(entry.text("path"), fontFamily = FontFamily.Monospace)
                        Text("${entry.text("status")} · +${entry.number("additions")} −${entry.number("deletions")}", color = MeshColors.secondary)
                    }
                }
            }
        }
    }
}

@Composable
fun DiffScreen(api: BuildmeshApi, id: Long, file: String, vm: BuildmeshViewModel) {
    val url = (api.origin + "/api/agents/$id/diff").toHttpUrl().newBuilder().addQueryParameter("path", file).build()
    var reload by remember { mutableIntStateOf(0) }
    val load by rememberRemote(api, url.encodedPath + "?" + url.encodedQuery, vm, reload) { response ->
        response.obj().arrayAt("files").flatMap { it.arrayAt("hunks") }.map { it to it.arrayAt("lines") }
    }
    val hunks = load.data
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        item { SelectionContainer { Text(file, fontFamily = FontFamily.Monospace) } }
        item { LoadNotice(load) { reload++ } }
        hunks?.forEach { (hunk, lines) ->
            item { Text("@@ −${hunk.number("old_start")} +${hunk.number("new_start")} @@", color = MeshColors.accent, fontFamily = FontFamily.Monospace) }
            items(lines) { line ->
                val kind = line.text("line_type")
                val prefix = when (kind) { "add" -> "+"; "remove" -> "−"; else -> " " }
                SelectionContainer { Text(prefix + line.text("content"), fontFamily = FontFamily.Monospace,
                    color = when (kind) { "add" -> MeshColors.green; "remove" -> MeshColors.red; else -> MeshColors.text }) }
            }
        }
        if (hunks != null && hunks.isEmpty()) item { EmptyMessage("No diff is available for this file.") }
    }
}

@Composable
fun CreatePrScreen(api: BuildmeshApi, meshId: Long, nodeId: Long, vm: BuildmeshViewModel) {
    val state by vm.state.collectAsState()
    var reload by remember { mutableIntStateOf(0) }
    val sourceLoad by rememberRemote(api, "/api/meshes/$meshId/pr/source?node_id=$nodeId", vm, reload, decode = ::prSource)
    val source = sourceLoad.data
    var title by rememberSaveable(state.draftNamespace, nodeId) { mutableStateOf(vm.draft("pr-title-$nodeId")) }
    var body by rememberSaveable(state.draftNamespace, nodeId) { mutableStateOf(vm.draft("pr-body-$nodeId")) }
    var base by rememberSaveable(state.draftNamespace, nodeId) { mutableStateOf("") }
    var result by rememberSaveable(state.draftNamespace, nodeId) { mutableStateOf("") }
    LaunchedEffect(source) { if (base.isEmpty()) source?.let { base = it.base } }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        LoadNotice(sourceLoad) { reload++ }
        source?.let { Text("${it.head} → ${base.ifBlank { it.base }}", fontFamily = FontFamily.Monospace, color = MeshColors.secondary) }
        OutlinedTextField(colors = buildmeshTextFieldColors(), enabled = !state.busy, value = title, onValueChange = { title = it; vm.saveDraft("pr-title-$nodeId", it) }, label = { Text("Title") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(colors = buildmeshTextFieldColors(), enabled = !state.busy, value = body, onValueChange = { body = it; vm.saveDraft("pr-body-$nodeId", it) }, label = { Text("Description") }, minLines = 4, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(colors = buildmeshTextFieldColors(), enabled = !state.busy && source != null, value = base, onValueChange = { base = it }, label = { Text("Base branch") }, modifier = Modifier.fillMaxWidth())
        Button(shape = MaterialTheme.shapes.small, enabled = !state.busy && source != null && title.isNotBlank() && result.isEmpty(), onClick = {
            val previewed = source ?: return@Button
            vm.action { client ->
                val auth = client.get("/api/gh/auth").obj()
                check(auth.flag("ok")) { "Sign in to GitHub with gh on the desktop first." }
                result = client.post("/api/meshes/$meshId/pr", pullRequestBody(nodeId, title, body, previewed, base)).obj().text("url")
                vm.saveDraft("pr-title-$nodeId", ""); vm.saveDraft("pr-body-$nodeId", "")
                vm.notice("Pull request created")
            }
        }, modifier = Modifier.fillMaxWidth()) { Text("Create pull request") }
        if (result.isNotEmpty()) ExternalLink(result, "Open pull request")
    }
}

@Composable
internal fun ExternalLink(url: String, label: String) {
    val handler = LocalUriHandler.current
    TextButton(onClick = { if (url.startsWith("https://")) handler.openUri(url) }) { Text(label) }
}
