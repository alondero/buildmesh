package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
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
        {"id":"config:saved","label":"Saved agent","resumable":true,"capabilities":{"supports_prefill":true}},
        {"id":"missing","label":"Unavailable agent","unavailable_reason":"Missing executable","resumable":true,"capabilities":{"supports_prefill":true}}
    ]""").objects()
    private val created = """{"id":17,"mesh_id":3,"name":"Created agent","provider":"config:saved","status":"idle"}"""

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
            requests.forEach { assertEquals(Json.parseToJsonElement("""{"mesh_id":3,"provider":"config:saved","rows":24,"cols":80,"prompt":"Fix the deadline"}"""), Json.parseToJsonElement(it.body.readUtf8())) }
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
            compose.setContent { BuildmeshTheme { NodeScreen(node, vm, {}, {}) } }
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
                "/api/meshes/3/issues" -> MockResponse().setBody("""[{"number":42,"title":"Fix deadline","body":"Issue details"}]""")
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
            compose.onNodeWithText("Agent: Saved agent").assertExists()
            compose.onNodeWithText("Start").performClick()
            compose.waitUntil(10000) { opened == 17L }
            val request = requests.single { it.method == "POST" }
            assertEquals("/api/meshes/3/issues/42/spawn", request.path)
            assertEquals("""{"title":"Fix deadline","provider":"config:saved"}""", request.body.readUtf8())
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
            compose.onNodeWithText("Agent: Saved agent").assertExists()
            compose.onNodeWithText("Start").performClick()
            compose.waitUntil(10000) { vm.state.value.error.contains("Node 17 was imported") && !vm.state.value.busy }
            compose.onNodeWithText("Choose a launch configuration").assertDoesNotExist()
            assertEquals(0L, opened)
            assertTrue(vm.state.value.error.contains("Executable missing"))
            val request = requests.single { it.method == "POST" }
            assertEquals("""{"cli_session_id":"saved-session","branch":"topic","provider":"config:saved","worktree_name":"tree"}""", request.body.readUtf8())
        }
    }

    @Test fun pullRequestChecksDesktopAuthenticationAndKeepsDraftUntilSuccess() {
        var checks = 0
        withDesktop({ request ->
            when (request.path) {
                "/api/gh/auth" -> MockResponse().setBody(if (++checks == 1) """{"ok":false}""" else """{"ok":true}""")
                "/api/meshes/3/pr" -> MockResponse().setBody("""{"url":"https://example.invalid/pull/17"}""")
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { CreatePrScreen(3, vm) } }
            compose.onNodeWithText("Title").performTextInput("Native change")
            compose.onNodeWithText("Description").performTextInput("Explains the change")
            compose.onNodeWithText("Create pull request").performScrollTo().performClick()
            compose.waitUntil(10000) { vm.state.value.error.contains("Sign in to GitHub") && !vm.state.value.busy }
            assertFalse(requests.any { it.method == "POST" })
            assertEquals("Native change", vm.draft("pr-title-3"))
            compose.onNodeWithText("Create pull request").performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Open pull request").fetchSemanticsNodes().isNotEmpty() }
            assertEquals("", vm.draft("pr-title-3"))
            assertEquals("", vm.draft("pr-body-3"))
            val request = requests.single { it.method == "POST" }
            assertEquals("/api/meshes/3/pr", request.path)
            assertEquals("""{"title":"Native change","body":"Explains the change","base_branch":"main"}""", request.body.readUtf8())
        }
    }
}
