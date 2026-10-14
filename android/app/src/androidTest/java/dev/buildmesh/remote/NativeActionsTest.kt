package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import java.util.concurrent.ConcurrentLinkedQueue

class NativeActionsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private val mesh = Json.parseToJsonElement("""{"id":3,"name":"QA mesh","path":"/repo"}""").obj()
    private val providers = Json.parseToJsonElement("""[
        {"id":"terminal","label":"Terminal","resumable":false,"capabilities":{"supports_prefill":false}},
        {"id":"launch/saved","label":"Saved agent","resumable":true,"capabilities":{"supports_prefill":true},"configuration":{"id":"launch/saved","spawn_option_id":"claude","name":"Saved agent"}},
        {"id":"claude","label":"Claude Code","resumable":true,"capabilities":{"supports_prefill":true}},
        {"id":"missing","label":"Unavailable agent","unavailable_reason":"Missing executable","resumable":true,"capabilities":{"supports_prefill":true}}
    ]""").objects()
    private val created = """{"id":17,"mesh_id":3,"name":"Created agent","provider":"claude","status":"idle"}"""

    private fun withDesktop(respond: (RecordedRequest) -> MockResponse, body: (BuildmeshViewModel, ConcurrentLinkedQueue<RecordedRequest>) -> Unit) {
        val app = ApplicationProvider.getApplicationContext<Application>()
        app.getSharedPreferences("drafts", 0).edit().clear().commit()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            val requests = ConcurrentLinkedQueue<RecordedRequest>()
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    if (request.path == "/api/session") return MockResponse().setResponseCode(204)
                    if (request.path == "/api/agents/9/git/branch") return MockResponse().setBody("""{"branch":"main"}""")
                    if (request.path == "/api/meshes/0/pulls?state=open") return MockResponse().setBody("""{"items":[],"completeness":{"returned":0,"pages_fetched":0,"complete":true,"incomplete_reason":null,"reported_total":null}}""")
                    requests.add(request)
                    return respond(request)
                }
            }
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            lateinit var vm: BuildmeshViewModel
            compose.runOnUiThread { vm = BuildmeshViewModel(app) }
            try {
                compose.waitUntil(10000) { !vm.state.value.restoring }
                body(vm, requests)
            } finally { compose.runOnUiThread { vm.forget() } }
        }
    }

    @Test fun captureFiltersConfigurationsAndKeepsDraftUntilAnAcknowledgedSuccess() {
        var attempts = 0
        var opened = 0L
        withDesktop({ request ->
            assertEquals("/api/nodes/create", request.path)
            if (++attempts == 1) MockResponse().setResponseCode(503).setBody("""{"error":"Retry this task"}""")
            else MockResponse().setBody(created)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                TaskScreen(state.copy(meshes = listOf(mesh), providers = providers), vm, 3, {}, { opened = it })
            } }
            compose.onNodeWithText("What should the agent do?").performTextInput("Fix the deadline")
            compose.waitUntil(5000) { compose.onAllNodesWithText("Launch configuration: Saved agent").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Start agent").performScrollTo().performClick()
            compose.waitUntil(10000) { vm.state.value.error == "Retry this task" && !vm.state.value.busy }
            compose.onNodeWithText("Fix the deadline").assertExists()
            assertEquals("Fix the deadline", vm.draft("capture"))
            assertEquals(0L, opened)
            compose.onNodeWithText("Start agent").performClick()
            compose.waitUntil(10000) { opened == 17L && !vm.state.value.busy }
            assertEquals("", vm.draft("capture"))
            assertTrue(vm.state.value.nodes.any { it.number("id") == 17L })
            assertEquals(2, requests.size)
            requests.forEach { assertEquals(Json.parseToJsonElement("""{"mesh_id":3,"provider":"claude","configuration_id":"launch/saved","rows":24,"cols":80,"prompt":"Fix the deadline"}"""), Json.parseToJsonElement(it.body.readUtf8())) }
        }
    }

    @Test fun replyFailurePreservesTextAndSuccessAcknowledgesTheCurrentRequest() {
        var attempts = 0
        val node = Json.parseToJsonElement("""{"id":9,"name":"Waiting agent","status":"awaiting_input","lifecycle":{"status":"awaiting_input","timestamp":"request-1","request":{"choices":["Proceed","Wait"]}}}""").obj()
        withDesktop({ request ->
            assertEquals("/api/nodes/9/input", request.path)
            if (++attempts == 1) MockResponse().setResponseCode(503).setBody("""{"error":"Reply not confirmed"}""")
            else MockResponse().setResponseCode(204)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { NodeScreen(node, vm, {}, {}, {}) } }
            compose.onNodeWithText("Your reply").performTextInput("Continue safely")
            compose.onNodeWithText("Send reply").performScrollTo().performClick()
            compose.waitUntil(10000) { vm.state.value.error == "Reply not confirmed" && !vm.state.value.busy }
            assertEquals("Continue safely", vm.draft("reply-9"))
            compose.onNodeWithText("Continue safely").assertExists()
            compose.onNodeWithText("Send reply").performClick()
            compose.waitUntil(10000) { vm.state.value.notice == "Reply delivered" && !vm.state.value.busy }
            compose.onNodeWithText("Send reply").assertDoesNotExist()
            assertEquals("", vm.draft("reply-9"))
            requests.forEach { assertEquals("""{"seq":"Continue safely\r"}""", it.body.readUtf8()) }
        }
    }

    @Test fun issueSpawnUsesAnAvailablePrefillConfigurationAndTheStrictPayload() {
        var opened = 0L
        withDesktop({ request ->
            when (request.path) {
                "/api/meshes/3/issues" -> MockResponse().setBody("""{"items":[{"number":42,"title":"Fix deadline","body":"Issue details","url":"https://github.com/o/r/issues/42","state":"open","labels":[],"blocked_by":[],"author":"octo"}],"completeness":{"returned":1,"pages_fetched":1,"complete":true,"incomplete_reason":null,"reported_total":null}}""")
                "/api/meshes/3/issues/42/spawn" -> MockResponse().setBody(created)
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(providers = providers), vm, { opened = it })
            } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("Start agent").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Start agent").performClick()
            compose.onNodeWithText("Agent: Claude Code").assertExists()
            compose.onNodeWithText("Start").performClick()
            compose.waitUntil(10000) { opened == 17L }
            val request = requests.single { it.method == "POST" }
            assertEquals("/api/meshes/3/issues/42/spawn", request.path)
            assertEquals("""{"title":"Fix deadline","provider":"claude"}""", request.body.readUtf8())
        }
    }

    @Test fun anIssuesResponseThisBuildCannotReadIsShownAsAnErrorInsteadOfCrashing() {
        withDesktop({ MockResponse().setBody("""{"unexpected":true}""") }) { vm, _ ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(providers = providers), vm, {})
            } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("Retry").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("The desktop sent an unexpected issues response. Update Buildmesh on the desktop or on this phone.").assertExists()
            compose.onNodeWithText("Start agent").assertDoesNotExist()
        }
    }

    @Test fun partialArchiveResumeDismissesTheDialogAndReportsTheImportedNode() {
        var opened = 0L
        withDesktop({ request ->
            when (request.path) {
                "/api/meshes/3/agent-nodes/discover" -> MockResponse().setBody("""[{"session_id":"saved-session","branch":"topic","worktree_name":"tree","first_message":"Archived work"}]""")
                "/api/meshes/3/agent-nodes/import-and-resume" -> MockResponse().setResponseCode(207).setBody("""{"node":{"id":17},"spawn_error":"Executable missing"}""")
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "archive", state.copy(providers = providers), vm, { opened = it })
            } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("Import and resume").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Import and resume").performClick()
            compose.onNodeWithText("Agent: Claude Code").assertExists()
            compose.onNodeWithText("Start").performClick()
            compose.waitUntil(10000) { vm.state.value.error.contains("Node 17 was imported") && !vm.state.value.busy }
            compose.onNodeWithText("Choose a launch configuration").assertDoesNotExist()
            assertEquals(0L, opened)
            assertTrue(vm.state.value.error.contains("Executable missing"))
            val request = requests.single { it.method == "POST" }
            assertEquals("""{"cli_session_id":"saved-session","branch":"topic","provider":"claude","worktree_name":"tree"}""", request.body.readUtf8())
        }
    }

    @Test fun pullRequestChecksDesktopAuthenticationAndKeepsDraftUntilSuccess() {
        var checks = 0
        withDesktop({ request ->
            when (request.path) {
                "/api/gh/auth" -> MockResponse().setBody(if (++checks == 1) """{"ok":false}""" else """{"ok":true}""")
                "/api/meshes/3/pr/source?node_id=17" -> MockResponse().setBody("""{"head_branch":"topic","base_branch":"main"}""")
                "/api/meshes/3/pr" -> MockResponse().setBody("""{"url":"https://example.invalid/pull/17"}""")
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { CreatePrScreen(vm.api!!, 3, 17, vm) } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("topic → main").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Title").performTextInput("Native change")
            compose.onNodeWithText("Description").performTextInput("Explains the change")
            compose.onNodeWithText("Create pull request").performScrollTo().performClick()
            compose.waitUntil(10000) { vm.state.value.error.contains("Sign in to GitHub") && !vm.state.value.busy }
            assertFalse(requests.any { it.method == "POST" })
            assertEquals("Native change", vm.draft("pr-title-17"))
            compose.onNodeWithText("Create pull request").performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Open pull request").fetchSemanticsNodes().isNotEmpty() }
            assertEquals("", vm.draft("pr-title-17"))
            assertEquals("", vm.draft("pr-body-17"))
            val request = requests.single { it.method == "POST" }
            assertEquals("/api/meshes/3/pr", request.path)
            assertEquals("""{"title":"Native change","body":"Explains the change","node_id":17,"head_branch":"topic","base_branch":"main"}""", request.body.readUtf8())
        }
    }

    private fun pullResponse(items: String, complete: Boolean = true) = """{"items":$items,"completeness":{"returned":1,"pages_fetched":1,"complete":$complete,"incomplete_reason":${if (complete) "null" else "\"safety_cap\""},"reported_total":null}}"""
    private val openPull = """[{"number":42,"title":"Fix search","body":"","url":"https://github.com/o/r/pull/42","state":"open","draft":false,"head_ref":"feature/search",
        "head_repo_owner":"o","head_repo_clone_url":"https://github.com/o/r.git","head_sha":"abc","author":"o","mergeable":true,"mergeable_state":"clean","checks_state":"success","review_decision":"approved"}]"""

    @Test fun aNodePullShowsStatusConfirmsSquashAndRemovesAnAcknowledgedMerge() {
        var merged = false
        withDesktop({ request -> when (request.path) {
            "/api/agents/17/git/branch" -> MockResponse().setBody("""{"branch":"feature/search"}""")
            "/api/meshes/3/pulls?state=open" -> MockResponse().setBody(pullResponse(if (merged) "[]" else openPull))
            "/api/meshes/3/pulls/42/mergeability" -> MockResponse().setBody("""{"mergeable":true,"mergeable_state":"clean"}""")
            "/api/meshes/3/pulls/42/merge" -> { merged = true; MockResponse().setBody("""{"url":"https://github.com/o/r/pull/42","pr_number":42}""") }
            "/api/nodes", "/api/meshes", "/api/providers" -> MockResponse().setBody("[]")
            else -> MockResponse().setResponseCode(404)
        } }) { vm, requests ->
            val node = Json.parseToJsonElement("""{"id":17,"mesh_id":3,"name":"Fix search","branch":"trunk","provider":"terminal","status":"idle"}""").obj()
            compose.setContent { BuildmeshTheme { NodeScreen(node, vm, {}, {}, {}) } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("Checks: passing").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Review: approved").assertExists()
            compose.onNodeWithText("Squash and merge").performScrollTo().performClick()
            compose.onNodeWithText("Squash-merge PR #42 and delete its branch on GitHub?").assertExists()
            assertFalse(requests.any { it.method == "POST" })
            compose.onNodeWithText("Cancel").performClick()
            compose.onNodeWithText("Squash and merge").performClick()
            compose.onNode(hasClickAction() and hasText("Confirm squash merge")).performClick()
            compose.waitUntil(10000) { merged && !vm.state.value.busy && compose.onAllNodesWithText("PR #42: Fix search").fetchSemanticsNodes().isEmpty() }
            assertEquals("PR #42 squash-merged. Branch deleted.", vm.state.value.notice)
            val post = requests.single { it.method == "POST" }
            assertEquals("/api/meshes/3/pulls/42/merge", post.path)
            assertEquals("""{"url":"https://github.com/o/r/pull/42"}""", post.body.readUtf8())
        }
    }

    @Test fun meshPullsDiscloseTruncationAndNodeMergeBecomesDisabledAfterStatusRefresh() {
        var blocked = false
        withDesktop({ request -> when (request.path) {
            "/api/agents/17/git/branch" -> MockResponse().setBody("""{"branch":"feature/search"}""")
            "/api/meshes/3/pulls?state=open" -> MockResponse().setBody(pullResponse(if (blocked) openPull.replace("\"clean\"", "\"blocked\"") else openPull, false))
            else -> MockResponse().setResponseCode(404)
        } }) { vm, requests ->
            var nodeId by androidx.compose.runtime.mutableStateOf<Long?>(null)
            compose.setContent { BuildmeshTheme { PullRequestsPanel(vm.api!!, 3, vm, nodeId) } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("Checks: passing").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Showing first 1 pull requests. Results are incomplete (safety_cap).").assertExists()
            compose.onNodeWithText("Squash and merge").assertDoesNotExist()
            compose.runOnIdle { nodeId = 17 }
            compose.waitUntil(10000) { compose.onAllNodesWithText("Squash and merge").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Squash and merge").assertIsEnabled()
            blocked = true
            compose.onNodeWithText("Refresh PR status").performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("feature/search · open · Blocked by required checks or reviews").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Squash and merge").assertIsNotEnabled()
            assertFalse(requests.any { it.method == "POST" })
        }
    }

    @Test fun draftsStayWithTheirDesktopWhenIdsRepeatAndReturnAfterRepairingTheSameDesktop() {
        withDesktop({ MockResponse().setResponseCode(204) }) { desktopA, _ ->
            val app = ApplicationProvider.getApplicationContext<Application>()
            val store = SessionStore(app)
            val sessionA = store.read()!!
            val key = "pr-title-3"
            desktopA.saveDraft(key, "Private title from desktop A")
            MockWebServer().use { serverB ->
                serverB.dispatcher = object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest) = MockResponse().setResponseCode(204)
                }
                serverB.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
                val sessionB = DeviceSession(serverB.url("/").toString(), null, "bm_session=desktop-b; Path=/; HttpOnly")
                store.save(sessionB)
                var desktopB: BuildmeshViewModel? = null
                var repairedA: BuildmeshViewModel? = null
                var repairedB: BuildmeshViewModel? = null
                try {
                    compose.runOnUiThread { desktopB = BuildmeshViewModel(app) }
                    val second = desktopB!!
                    compose.waitUntil(10000) { !second.state.value.restoring }
                    assertEquals("Desktop B must not show desktop A's draft when mesh IDs match", "", second.draft(key))
                    second.saveDraft(key, "Private title from desktop B")

                    store.save(sessionA)
                    compose.runOnUiThread { repairedA = BuildmeshViewModel(app) }
                    val secondA = repairedA!!
                    compose.waitUntil(10000) { !secondA.state.value.restoring }
                    assertEquals("Re-pairing desktop A should restore its own draft", "Private title from desktop A", secondA.draft(key))

                    store.save(sessionB)
                    compose.runOnUiThread { repairedB = BuildmeshViewModel(app) }
                    val secondB = repairedB!!
                    compose.waitUntil(10000) { !secondB.state.value.restoring }
                    assertEquals("Re-pairing desktop B should restore only its own draft", "Private title from desktop B", secondB.draft(key))
                } finally {
                    compose.runOnUiThread {
                        desktopB?.forget()
                        repairedA?.forget()
                        repairedB?.forget()
                    }
                    app.getSharedPreferences("drafts", 0).edit().clear().commit()
                }
            }
        }
    }
}
