package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.serialization.json.*

// Fields follow GitHubPullRequestFeed / GitHubPullRequest in src/types/generated.
data class PhonePullRequest(val number: Long, val title: String, val url: String, val head: String,
    val headRepo: String, val state: String, val draft: Boolean, val mergeable: Boolean?,
    val mergeState: String, val checks: String?, val review: String?) {
    val canMerge get() = state == "open" && !draft && mergeable == true &&
        mergeState in setOf("clean", "has_hooks", "unstable") &&
        checks !in setOf("failure", "error", "pending", "expected") && review !in setOf("changes_requested", "review_required")
    val mergeLabel get() = when {
        draft -> "Draft"
        mergeable == null -> "Checking mergeability"
        mergeable == false || mergeState == "dirty" -> "Conflicts"
        mergeState == "blocked" -> "Blocked by required checks or reviews"
        mergeState == "behind" -> "Branch is behind base"
        mergeState == "unstable" -> "Checks need attention"
        mergeState in setOf("clean", "has_hooks") -> "Mergeable"
        else -> "Mergeability unknown"
    }
}

data class PhonePullFeed(val items: List<PhonePullRequest>, val complete: Boolean, val returned: Long,
    val total: Long?, val reason: String)

fun pullFeed(response: JsonElement): PhonePullFeed {
    val obj = response.obj()
    val completeness = obj.objectAt("completeness") ?: unexpectedResponse("pull requests")
    val complete = (completeness["complete"] as? JsonPrimitive)?.booleanOrNull ?: unexpectedResponse("pull requests")
    val items = (obj["items"] as? JsonArray ?: unexpectedResponse("pull requests")).map { element ->
        val pr = element.obj()
        val number = pr.number("number").takeIf { it > 0 } ?: unexpectedResponse("pull request")
        val draft = (pr["draft"] as? JsonPrimitive)?.booleanOrNull ?: unexpectedResponse("pull request")
        val mergeable = when (val value = pr["mergeable"]) {
            null, JsonNull -> null
            else -> (value as? JsonPrimitive)?.booleanOrNull ?: unexpectedResponse("mergeability")
        }
        PhonePullRequest(number, pr.text("title"), pr.text("url"), pr.text("head_ref"),
            pr.text("head_repo_clone_url"), pr.text("state"), draft, mergeable, pr.text("mergeable_state"),
            (pr["checks_state"] as? JsonPrimitive)?.contentOrNull,
            (pr["review_decision"] as? JsonPrimitive)?.contentOrNull)
    }
    return PhonePullFeed(items, complete, completeness.number("returned"),
        (completeness["reported_total"] as? JsonPrimitive)?.longOrNull, completeness.text("incomplete_reason"))
}

fun nodePulls(feed: PhonePullFeed, branch: String) = feed.items.filter {
    branch.isNotEmpty() && it.head == branch && it.headRepo == it.url.substringBefore("/pull/") + ".git"
}

fun checkedMergeability(pr: PhonePullRequest, response: JsonElement): PhonePullRequest {
    val obj = response.obj()
    val mergeable = when (val value = obj["mergeable"]) {
        JsonNull -> null
        else -> (value as? JsonPrimitive)?.booleanOrNull ?: unexpectedResponse("mergeability")
    }
    return pr.copy(mergeable = mergeable, mergeState = obj.text("mergeable_state"))
}

suspend fun BuildmeshApi.squashNodePull(meshId: Long, pr: PhonePullRequest) {
    check(pr.canMerge) { "This pull request cannot be merged yet." }
    val latest = checkedMergeability(pr, get("/api/meshes/$meshId/pulls/${pr.number}/mergeability"))
    check(latest.canMerge) { "This pull request is no longer mergeable. Refresh its status." }
    post("/api/meshes/$meshId/pulls/${pr.number}/merge", buildJsonObject { put("url", pr.url) })
}

@Composable
fun PullRequestsScreen(api: BuildmeshApi, meshId: Long, vm: BuildmeshViewModel) {
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)) {
        PullRequestsPanel(api, meshId, vm)
    }
}

@Composable
fun PullRequestsPanel(api: BuildmeshApi, meshId: Long, vm: BuildmeshViewModel, nodeId: Long? = null) {
    key(api, meshId, nodeId) { OwnedPullRequestsPanel(api, meshId, vm, nodeId) }
}

@Composable
private fun OwnedPullRequestsPanel(api: BuildmeshApi, meshId: Long, vm: BuildmeshViewModel, nodeId: Long?) {
    val state by vm.state.collectAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var reload by remember { mutableIntStateOf(0) }
    var feed by remember { mutableStateOf<PhonePullFeed?>(null) }
    var branch by remember { mutableStateOf("") }
    var error by remember { mutableStateOf("") }
    var confirming by remember { mutableStateOf<PhonePullRequest?>(null) }
    var merging by remember { mutableStateOf(false) }
    LaunchedEffect(api, reload, state.resourceRevision, lifecycle, merging) {
        if (merging) return@LaunchedEffect
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                try {
                    val currentBranch = if (nodeId == null) "" else api.get("/api/agents/$nodeId/git/branch").obj().text("branch")
                    val next = pullFeed(api.get("/api/meshes/$meshId/pulls?state=open"))
                    confirming?.let { previous ->
                        if (currentBranch != branch || next.items.none { it.number == previous.number && it.canMerge && it == previous }) confirming = null
                    }
                    feed = next; branch = currentBranch; error = ""
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) { feed = null; confirming = null; error = friendlyError(e); vm.report(e) }
                delay(10_000)
            }
        }
    }
    val prs = feed?.let { if (nodeId == null) it.items else nodePulls(it, branch) }.orEmpty()
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(if (nodeId == null) "Open pull requests" else "This node’s pull request", style = MaterialTheme.typography.titleMedium)
        TextButton(enabled = !merging, onClick = { reload++; confirming = null }) { Text("Refresh PR status") }
        if (error.isNotEmpty()) Text(error, color = MeshColors.amber)
        if (feed == null && error.isEmpty()) Text("Loading pull requests…")
        feed?.let { current ->
            if (!current.complete) Text("Showing first ${current.returned}${current.total?.let { " of $it" }.orEmpty()} pull requests. Results are incomplete (${current.reason})." +
                if (nodeId != null) " This node’s PR may be missing from this list." else "", color = MeshColors.amber)
            if (prs.isEmpty()) Text(if (nodeId == null) "No open pull requests in the returned results." else if (branch.isEmpty()) "The node has no current branch." else "No open pull request found for $branch in the returned results.")
        }
        prs.forEach { pr ->
            Card(colors = CardDefaults.cardColors(containerColor = MeshColors.card)) {
                Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("PR #${pr.number}: ${pr.title}", style = MaterialTheme.typography.titleMedium)
                    Text("${pr.head} · ${pr.state} · ${pr.mergeLabel}")
                    Text("Checks: ${when (pr.checks) { "success" -> "passing"; "failure" -> "failing"; null -> "not reported"; else -> pr.checks }}")
                    Text("Review: ${pr.review?.replace('_', ' ') ?: "no decision reported"}")
                    if (pr.url.startsWith("https://")) ExternalLink(pr.url, "Open on GitHub")
                    if (nodeId != null) Button(shape = MaterialTheme.shapes.small, enabled = !state.busy && !merging && pr.canMerge,
                        onClick = { confirming = pr }) { Text(if (merging) "Merging…" else "Squash and merge") }
                }
            }
        }
    }
    confirming?.let { pr ->
        AlertDialog(onDismissRequest = { if (!merging) confirming = null }, title = { Text("Confirm squash merge") },
            text = { Text("Squash-merge PR #${pr.number} and delete its branch on GitHub?") },
            confirmButton = { TextButton(enabled = !state.busy && !merging && pr.canMerge, onClick = {
                merging = true
                vm.action { client ->
                    try {
                        client.squashNodePull(meshId, pr)
                        feed = feed?.let { it.copy(items = it.items.filterNot { item -> item.number == pr.number }) }
                        vm.notice("PR #${pr.number} squash-merged. Branch deleted.")
                    } finally { merging = false; confirming = null; reload++ }
                }
            }) { Text("Confirm squash merge") } },
            dismissButton = { TextButton(enabled = !merging, onClick = { confirming = null }) { Text("Cancel") } })
    }
}
