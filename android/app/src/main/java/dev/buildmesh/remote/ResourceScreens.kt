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

@Composable
private fun rememberRemote(api: BuildmeshApi, path: String, vm: BuildmeshViewModel, reload: Int = 0): State<RemoteLoad> {
    val state by vm.state.collectAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    return produceState(RemoteLoad(), api, path, reload, state.resourceRevision, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            value = RemoteLoad()
            try { value = RemoteLoad(loading = false, data = api.get(path)) }
            catch (e: CancellationException) { throw e }
            catch (e: Exception) { value = RemoteLoad(loading = false, error = friendlyError(e)); vm.report(e) }
            awaitCancellation()
        }
    }
}

private data class RemoteLoad(val loading: Boolean = true, val data: JsonElement? = null, val error: String = "")

@Composable
private fun LoadNotice(load: RemoteLoad, retry: () -> Unit) {
    if (load.loading) EmptyMessage("Loading…")
    else if (load.error.isNotEmpty()) Column {
        EmptyMessage(load.error)
        TextButton(onClick = retry) { Text("Retry") }
    }
}

@Composable
fun ResourceScreen(api: BuildmeshApi, meshId: Long, screen: String, state: RemoteState, vm: BuildmeshViewModel, openNode: (Long) -> Unit) {
    var reload by remember { mutableIntStateOf(0) }
    val path = "/api/meshes/$meshId/" + if (screen == "issues") "issues" else "agent-nodes/discover"
    val load by rememberRemote(api, path, vm, reload)
    val data = load.data
    var selected by remember { mutableStateOf<JsonObject?>(null) }
    var provider by rememberSaveable { mutableStateOf("") }
    val options = state.providers.filter {
        it.text("unavailable_reason").isEmpty() &&
            if (screen == "archive") it.flag("resumable") else it.objectAt("capabilities")?.flag("supports_prefill") == true
    }
    LaunchedEffect(options) { if (options.none { it.text("id") == provider }) provider = options.firstOrNull()?.text("id").orEmpty() }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { TextButton(onClick = { reload++ }) { Text("Reload ${if (screen == "issues") "issues" else "archive"}") } }
        item { LoadNotice(load) { reload++ } }
        val resources = data?.objects().orEmpty()
        if (data != null && resources.isEmpty()) item { EmptyMessage("No ${if (screen == "issues") "open issues" else "archived sessions"} found.") }
        items(resources) { item ->
            Card(colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(if (screen == "issues") "#${item.number("number")} ${item.text("title")}" else item.text("first_message").ifEmpty { "Archived session" }, style = MaterialTheme.typography.titleMedium)
                    Text(if (screen == "issues") item.text("body").take(500) else "${item.text("branch")} · ${item.text("timestamp")}", color = MeshColors.secondary)
                    Button(enabled = !state.busy && options.isNotEmpty(), onClick = { selected = item }) { Text(if (screen == "issues") "Start agent" else "Import and resume") }
                    if (screen == "issues" && item.text("url").startsWith("https://")) ExternalLink(item.text("url"), "View issue")
                }
            }
        }
    }
    selected?.let { item ->
        AlertDialog(onDismissRequest = { if (!state.busy) selected = null }, title = { Text("Choose a launch configuration") },
            text = { Selector("Agent", options.map { it.text("id") to it.text("label") }, provider, { provider = it }) },
            confirmButton = { TextButton(enabled = !state.busy && provider.isNotEmpty(), onClick = {
                vm.action { client ->
                    val node = try { if (screen == "issues") client.post("/api/meshes/$meshId/issues/${item.number("number")}/spawn", buildJsonObject {
                        put("title", item.text("title")); put("provider", provider)
                    }) else client.post("/api/meshes/$meshId/agent-nodes/import-and-resume", buildJsonObject {
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
    var reload by remember { mutableIntStateOf(0) }
    val changesLoad by rememberRemote(api, "/api/agents/$id/git/status", vm, reload)
    val branchLoad by rememberRemote(api, "/api/agents/$id/git/branch", vm, reload)
    val changes = changesLoad.data
    val branch = branchLoad.data
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Text("Branch: ${branch?.obj()?.text("branch").orEmpty()}", color = MeshColors.secondary); TextButton(onClick = { reload++ }) { Text("Refresh changes") } }
        item { LoadNotice(changesLoad) { reload++ }; LoadNotice(branchLoad) { reload++ } }
        if (changes != null && changes.objects().isEmpty()) item { EmptyMessage("Working tree is clean.") }
        items(changes?.objects().orEmpty()) { entry ->
            Card(onClick = { openFile(entry.text("path")) }, colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                Column(Modifier.fillMaxWidth().padding(16.dp)) {
                    Text(entry.text("path"), fontFamily = FontFamily.Monospace)
                    Text("${entry.text("status")} · +${entry.number("additions")} −${entry.number("deletions")}", color = MeshColors.secondary)
                }
            }
        }
    }
}

@Composable
fun DiffScreen(api: BuildmeshApi, id: Long, file: String, vm: BuildmeshViewModel) {
    val url = (api.origin + "/api/agents/$id/diff").toHttpUrl().newBuilder().addQueryParameter("path", file).build()
    var reload by remember { mutableIntStateOf(0) }
    val load by rememberRemote(api, url.encodedPath + "?" + url.encodedQuery, vm, reload)
    val data = load.data
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        item { SelectionContainer { Text(file, fontFamily = FontFamily.Monospace) } }
        item { LoadNotice(load) { reload++ } }
        data?.obj()?.arrayAt("files")?.forEach { diff ->
            diff.arrayAt("hunks").forEach { hunk ->
                item { Text("@@ −${hunk.number("old_start")} +${hunk.number("new_start")} @@", color = MeshColors.accent, fontFamily = FontFamily.Monospace) }
                items(hunk.arrayAt("lines")) { line ->
                    val kind = line.text("line_type")
                    val prefix = when (kind) { "add" -> "+"; "remove" -> "−"; else -> " " }
                    SelectionContainer { Text(prefix + line.text("content"), fontFamily = FontFamily.Monospace,
                        color = when (kind) { "add" -> MeshColors.green; "remove" -> MeshColors.red; else -> MeshColors.text }) }
                }
            }
        }
        if (data != null && data.obj().arrayAt("files").isEmpty()) item { EmptyMessage("No diff is available for this file.") }
    }
}

@Composable
fun CreatePrScreen(meshId: Long, vm: BuildmeshViewModel) {
    val state by vm.state.collectAsState()
    var title by rememberSaveable(meshId) { mutableStateOf(vm.draft("pr-title-$meshId")) }
    var body by rememberSaveable(meshId) { mutableStateOf(vm.draft("pr-body-$meshId")) }
    var base by rememberSaveable(meshId) { mutableStateOf("main") }
    var result by rememberSaveable(meshId) { mutableStateOf("") }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).imePadding().padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text("Create a PR from the mesh's current branch. For an agent worktree, ask the agent to open its PR through the terminal.", color = MeshColors.secondary)
        OutlinedTextField(enabled = !state.busy, value = title, onValueChange = { title = it; vm.saveDraft("pr-title-$meshId", it) }, label = { Text("Title") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(enabled = !state.busy, value = body, onValueChange = { body = it; vm.saveDraft("pr-body-$meshId", it) }, label = { Text("Description") }, minLines = 4, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(enabled = !state.busy, value = base, onValueChange = { base = it }, label = { Text("Base branch") }, modifier = Modifier.fillMaxWidth())
        Button(enabled = !state.busy && title.isNotBlank() && base.isNotBlank() && result.isEmpty(), onClick = {
            vm.action { api ->
                val auth = api.get("/api/gh/auth").obj()
                check(auth.flag("ok")) { "Sign in to GitHub with gh on the desktop first." }
                result = api.post("/api/meshes/$meshId/pr", buildJsonObject { put("title", title.trim()); put("body", body); put("base_branch", base.trim()) }).obj().text("url")
                vm.saveDraft("pr-title-$meshId", ""); vm.saveDraft("pr-body-$meshId", "")
                vm.notice("Pull request created")
            }
        }, modifier = Modifier.fillMaxWidth()) { Text("Create pull request") }
        if (result.isNotEmpty()) ExternalLink(result, "Open pull request")
    }
}

@Composable
private fun ExternalLink(url: String, label: String) {
    val handler = LocalUriHandler.current
    TextButton(onClick = { if (url.startsWith("https://")) handler.openUri(url) }) { Text(label) }
}
