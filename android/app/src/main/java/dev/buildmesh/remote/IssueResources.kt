package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.*

fun issueFeed(response: JsonElement): JsonObject {
    val items = issueItems(response)
    return buildJsonObject {
        put("items", JsonArray(items))
        put("completeness", (response as? JsonObject)?.objectAt("completeness") ?: JsonNull)
    }
}

fun issueLabels(issue: JsonObject): List<String> = (issue["labels"] as? JsonArray).orEmpty().map { it.jsonPrimitive.content }

fun filterIssues(issues: List<JsonObject>, filter: String): List<JsonObject> {
    val query = filter.trim()
    return issues.filter { query.isEmpty() || it.text("title").contains(query, ignoreCase = true) || it.number("number").toString().contains(query.removePrefix("#")) || issueLabels(it).any { label -> label.contains(query, ignoreCase = true) } }
}

fun issueBlockers(issue: JsonObject, issues: List<JsonObject>): List<Long> {
    val open = issues.map { it.number("number") }.toSet()
    return (issue["blocked_by"] as? JsonArray).orEmpty().mapNotNull { it.jsonPrimitive.longOrNull }.filter { it in open }
}

fun issueCompletenessNote(feed: JsonObject): String? {
    val completeness = feed.objectAt("completeness") ?: return "Completeness is unavailable. More issues may exist."
    if (completeness.flag("complete")) return null
    val total = (completeness["reported_total"] as? JsonPrimitive)?.longOrNull
    val count = "Showing the first ${completeness.number("returned")}" + (total?.let { " of $it" } ?: "") + " issues."
    val reason = when (completeness.text("incomplete_reason")) {
        "safety_cap" -> "The read hit its page limit, so more exist."
        "search_ceiling" -> "GitHub's search API cannot return more than 1,000 matches for this query."
        "upstream_incomplete" -> "GitHub reported that its own index was still incomplete — try again."
        "cancelled" -> "The read was cancelled before it finished."
        else -> "More results may exist."
    }
    return "$count $reason"
}

/**
 * The mesh's open issues.
 *
 * The list refreshes by pulling down, and the app-bar action stays the
 * shared recovery affordance; a failed read keeps its own Retry, above the
 * fold, so the screen never needs a scroll to recover.
 */
@Composable
fun IssuesResourceScreen(api: BuildmeshApi, meshId: Long, state: RemoteState, vm: BuildmeshViewModel, openNode: (Long) -> Unit) {
    val refresh = remember { ListingRefresh() }
    val load by rememberRemote(api, "/api/meshes/$meshId/issues", vm, refresh.reload, onRead = { refresh.settle() }, decode = ::issueFeed)
    val issues = load.data?.arrayAt("items").orEmpty()
    var filter by rememberSaveable(state.draftNamespace, meshId) { mutableStateOf("") }
    var expanded by rememberSaveable(state.draftNamespace, meshId) { mutableStateOf<Long?>(null) }
    var editing by remember(state.draftNamespace, meshId) { mutableStateOf<JsonObject?>(null) }
    val options = launchOptions(state.providers, needsPrompt = true)
    val defaultName = meshDefaultName(state.meshes.firstOrNull { it.number("id") == meshId }, state.providers)
    // Like the desktop Issues probe, choosing a launch starts it: the node row
    // is committed at once and its status reports the background start.
    fun start(issue: JsonObject, option: JsonObject?) = vm.action { client ->
        val node = client.post("/api/meshes/$meshId/issues/${issue.number("number")}/spawn", issueSpawnBody(issue, option)).obj()
        vm.acceptNode(node); openNode(node.number("id"))
    }
    val filtered = filterIssues(issues, filter)
    PullToRefreshList(refresh, Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item { OutlinedTextField(colors = buildmeshTextFieldColors(), value = filter, onValueChange = { filter = it }, label = { Text("Filter issues") }, singleLine = true, modifier = Modifier.fillMaxWidth()) }
            item { LoadNotice(load) { refresh.request() } }
            load.data?.let { feed -> issueCompletenessNote(feed)?.let { note -> item { Text(note, color = MeshColors.secondary) } } }
            if (load.data != null && filtered.isEmpty()) item { EmptyMessage(if (issues.isEmpty()) "No open issues found." else "No matches.") }
            items(filtered, key = { it.number("number") }) { issue ->
                val number = issue.number("number")
                val open = expanded == number
                Card(onClick = { expanded = if (open) null else number }, colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("#$number ${issue.text("title")}", style = MaterialTheme.typography.titleMedium, maxLines = if (open) Int.MAX_VALUE else 2, overflow = TextOverflow.Ellipsis)
                        if (issue.text("author").isNotEmpty()) Text("@${issue.text("author")}", color = MeshColors.secondary)
                        val blockers = issueBlockers(issue, issues)
                        if (blockers.isNotEmpty()) Text("Blocked by " + blockers.joinToString(", ") { "#$it" }, color = MeshColors.red)
                        if (issueLabels(issue).isNotEmpty()) Text(issueLabels(issue).joinToString(" · "), color = MeshColors.accent)
                        SpawnSplitButton(defaultName, options, "issue #$number", !state.busy, { start(issue, null) }, { start(issue, it) })
                        TextButton(enabled = !state.busy, onClick = { editing = issue }) { Text("Edit tags for issue #$number") }
                        if (open) {
                            val body = issue.text("body").trim()
                            if (body.isNotEmpty()) Text(body.take(600) + if (body.length > 600) "…" else "", color = MeshColors.secondary, modifier = Modifier.heightIn(max = 180.dp).verticalScroll(rememberScrollState()))
                            if (issue.text("url").startsWith("https://")) ExternalLink(issue.text("url"), "View issue")
                        }
                    }
                }
            }
        }
    }
    editing?.let { issue ->
        IssueTagDialog(api, meshId, issue, state.busy, vm, onClose = { editing = null }, onChanged = { label, present ->
            editing = JsonObject(issue + ("labels" to JsonArray((issueLabels(issue).filterNot { it.equals(label, ignoreCase = true) } + if (present) listOf(label) else emptyList()).map(::JsonPrimitive))))
            refresh.request()
        })
    }
}

@Composable
private fun IssueTagDialog(api: BuildmeshApi, meshId: Long, issue: JsonObject, busy: Boolean, vm: BuildmeshViewModel, onClose: () -> Unit, onChanged: (String, Boolean) -> Unit) {
    var retry by remember { mutableIntStateOf(0) }
    val load by rememberRemote(api, "/api/meshes/$meshId/issue-labels", vm, retry) { it.obj() }
    var search by remember { mutableStateOf("") }
    var writeError by remember { mutableStateOf("") }
    val triggers = load.data?.objectAt("autopilot_triggers")
    fun watched(label: String) = (triggers?.get(label.lowercase()) as? JsonArray)?.map { it.jsonPrimitive.content }.orEmpty()
    val names = (load.data?.get("labels") as? JsonArray).orEmpty().map { it.jsonPrimitive.content }
    val choices = (issueLabels(issue) + names).distinct().filter { it.contains(search.trim(), ignoreCase = true) }.sortedWith(compareBy<String> { watched(it).isEmpty() }.thenBy { it.lowercase() })
    AlertDialog(onDismissRequest = { if (!busy) onClose() }, title = { Text("Tags for issue #${issue.number("number")}") },
        text = { Column(Modifier.fillMaxWidth().heightIn(max = 420.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(colors = buildmeshTextFieldColors(), value = search, onValueChange = { search = it }, label = { Text("Filter tags") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            LoadNotice(load) { retry++ }
            if (load.data != null) {
                if (choices.isEmpty()) Text(if (search.isEmpty()) "No repository labels" else "No matching tags")
                choices.forEach { label ->
                    val present = issueLabels(issue).any { it.equals(label, ignoreCase = true) }
                    Row(Modifier.fillMaxWidth()) {
                        Checkbox(modifier = Modifier.semantics { contentDescription = label }, checked = present, enabled = !busy, onCheckedChange = { checked ->
                            writeError = ""
                            vm.action { client ->
                                try {
                                    client.post("/api/meshes/$meshId/issues/${issue.number("number")}/label", buildJsonObject { put("label", label); put("present", checked) })
                                    onChanged(label, checked)
                                } catch (e: CancellationException) { throw e
                                } catch (e: Exception) { writeError = friendlyError(e); throw e }
                            }
                        })
                        Column(Modifier.weight(1f).padding(top = 12.dp)) {
                            Text(label)
                            if (watched(label).isNotEmpty()) Text("Autopilot: ${watched(label).joinToString(", ")}. Eligible for pickup on the next poll; dependencies and existing runs still apply.", style = MaterialTheme.typography.bodySmall, color = MeshColors.accent)
                        }
                    }
                }
            }
            if (busy) Text("Saving tags…")
            if (writeError.isNotEmpty()) Text(writeError, color = MeshColors.red)
        } }, confirmButton = { TextButton(enabled = !busy, onClick = onClose) { Text("Done") } })
}
