package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
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
            compose.waitUntil(10000) { compose.onAllNodesWithText("#42 Fix deadline").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Issue details").assertDoesNotExist()
            compose.onNodeWithText("#42 Fix deadline").performClick()
            compose.onNodeWithText("Issue details").assertExists()
            compose.onNodeWithText("Start agent").performClick()
            compose.onNodeWithText("Launch configuration: Mesh default").performClick()
            compose.onNodeWithText("Terminal").assertDoesNotExist()
            compose.onNodeWithText("Saved agent").assertDoesNotExist()
            compose.onNodeWithText("Unavailable agent").assertDoesNotExist()
            compose.onNodeWithText("Claude Code").performClick()
            compose.onNodeWithText("Launch configuration: Claude Code").assertExists()
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

    private fun issueScreenshot(name: String) {
        compose.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val image = instrumentation.uiAutomation.takeScreenshot()
        instrumentation.targetContext.filesDir.resolve("$name.png").outputStream().use {
            check(image.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it))
        }
        image.recycle()
    }

    @Test fun issueTriageFiltersMetadataShowsPartialFeedAndAcknowledgesTags() {
        var tagged = false
        var attempts = 0
        withDesktop({ request ->
            when (request.path) {
                "/api/meshes/3/issues" -> MockResponse().setBody("""{"items":[
                    {"number":42,"title":"Fix the mobile deadline and preserve a long issue title on a narrow phone screen","body":"Expanded issue details","url":"https://github.com/o/r/issues/42","state":"open","labels":["bug"${if (tagged) ",\"buildmesh:run\"" else ""}],"blocked_by":[43,99],"author":"octo"},
                    {"number":43,"title":"Prerequisite","body":"","url":"","state":"open","labels":[],"blocked_by":[],"author":""}
                ],"completeness":{"returned":2,"pages_fetched":1,"complete":false,"incomplete_reason":"safety_cap","reported_total":120}}""")
                "/api/meshes/3/issue-labels" -> MockResponse().setBody("""{"labels":["bug","buildmesh:run"],"autopilot_triggers":{"buildmesh:run":["Ship fixes"]}}""")
                "/api/meshes/3/issues/42/label" -> {
                    if (++attempts == 1) MockResponse().setResponseCode(500).setBody("""{"error":"GitHub permission denied"}""")
                    else { tagged = Json.parseToJsonElement(request.body.clone().readUtf8()).obj().flag("present"); MockResponse().setResponseCode(204) }
                }
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(providers = providers), vm, {})
            } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("@octo").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Expanded issue details").assertDoesNotExist()
            compose.onNodeWithText("Blocked by #43").assertExists()
            compose.onNodeWithText("Showing the first 2 of 120 issues.", substring = true).assertExists()
            issueScreenshot("issues-android-collapsed")
            compose.onNodeWithText("Filter issues").performTextInput(" BUG ")
            compose.onNodeWithText("Prerequisite", substring = true).assertDoesNotExist()
            compose.onNodeWithText("Blocked by #43").assertExists()
            compose.onNodeWithText("Filter issues").performTextClearance()
            compose.onNodeWithText("Filter issues").performTextInput("no match")
            compose.onNodeWithText("No matches.").assertExists()
            compose.onNodeWithText("Showing the first 2 of 120 issues.", substring = true).assertExists()
            compose.onNodeWithText("Filter issues").performTextClearance()
            compose.onNodeWithText("#42 Fix the mobile", substring = true).performClick()
            compose.onNodeWithText("Expanded issue details").assertExists()
            compose.onNodeWithText("Start agent").performScrollTo().performClick()
            compose.onNodeWithText("Launch configuration: Mesh default").assertExists()
            issueScreenshot("issues-android-launch")
            compose.onNodeWithText("Cancel").performClick()
            assertFalse(requests.any { it.method == "POST" })
            compose.onNodeWithText("Edit tags for issue #42").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithContentDescription("buildmesh:run").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Autopilot: Ship fixes.", substring = true).assertExists()
            issueScreenshot("issues-android-tags")
            compose.onNodeWithContentDescription("buildmesh:run").assertIsOff().performClick()
            compose.waitUntil(10000) { vm.state.value.error == "GitHub permission denied" && !vm.state.value.busy }
            compose.onNodeWithText("GitHub permission denied").assertExists()
            compose.onNodeWithContentDescription("buildmesh:run").assertIsOff().performClick()
            compose.waitUntil(10000) { tagged && !vm.state.value.busy }
            compose.onNodeWithContentDescription("buildmesh:run").assertIsOn()
            compose.onNodeWithContentDescription("bug").assertIsOn()
            compose.onNodeWithContentDescription("buildmesh:run").performClick()
            compose.waitUntil(10000) { attempts == 3 && !vm.state.value.busy }
            compose.onNodeWithContentDescription("buildmesh:run").assertIsOff()
            val writes = requests.filter { it.method == "POST" }
            assertEquals(3, writes.size)
            assertEquals(listOf(true, true, false), writes.map { Json.parseToJsonElement(it.body.readUtf8()).obj().flag("present") })
            assertTrue(writes.all { it.path == "/api/meshes/3/issues/42/label" })
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
            compose.onNodeWithText("Launch configuration: Claude Code").assertExists()
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
