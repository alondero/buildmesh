package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import java.util.concurrent.ConcurrentLinkedQueue

// The desktop's responses are read from the committed mobile-route fixtures
// (issue #2318), the same files the Rust gate derives from the wire types.
// A body written by hand here would keep passing when the desktop changed
// its shape, so the fixtures are the source of truth and only the values
// that belong to a test's own scenario are overlaid on them.
class NativeActionsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private val mesh = MobileWireFixtures.array("meshes").first().jsonObject
    private val providers = MobileWireFixtures.array("providers").map { it.jsonObject }
    private val created = MobileWireFixtures.raw("agent_node")
    // #2379 answers POST /api/nodes/create with a typed outcome, not a bare node.
    private val createActive = MobileWireFixtures.raw("create_node_outcome_active")
    /** The canonical issue row, so a renamed field breaks the triage test too. */
    private val issueRow = MobileWireFixtures.obj("issues_feed").arrayAt("items").first()
    /** The canonical pull request row, likewise. */
    private val pullRow = MobileWireFixtures.obj("pulls_feed").arrayAt("items").first()
    /**
     * The Spawn Option the archive dialog preselects: the first menu row
     * that is available, resumable and has no saved configuration of its
     * own. The plain terminal is not resumable, so it is not offered.
     */
    private val archiveProvider = providers
        .filter { it.flag("resumable") && it.objectAt("configuration") == null && it.text("unavailable_reason").isEmpty() }
        .first()
        .text("id")

    // The canonical feed's envelope, with this test's own rows: the triage
    // screen needs a truncated feed, a blocked issue and a long title.
    private val triageCompleteness = JsonObject(
        MobileWireFixtures.obj("issues_feed").objectAt("completeness")!!.mapValues { (key, value) ->
            when (key) {
                "returned" -> JsonPrimitive(2)
                "pages_fetched" -> JsonPrimitive(1)
                "complete" -> JsonPrimitive(false)
                "incomplete_reason" -> JsonPrimitive("safety_cap")
                "reported_total" -> JsonPrimitive(120)
                else -> value
            }
        },
    )

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
                    if (request.path == "/api/agents/9/git/branch") return MockResponse().setBody(MobileWireFixtures.raw("git_branch"))
                    if (request.path == "/api/meshes/0/pulls?state=open") return MockResponse().setBody(MobileWireFixtures.raw("pulls_feed_empty"))
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

    @Test fun closeRetainsTheAgentOnFailureAndRemovesItOnlyAfterAcknowledgement() {
        val node = MobileWireFixtures.obj("agent_node")
        val id = node.number("id")
        var attempts = 0
        withDesktop({ request ->
            assertEquals("/api/nodes/$id/close", request.path)
            assertEquals("POST", request.method)
            assertEquals("{}", request.body.readUtf8())
            if (++attempts == 1) MockResponse().setResponseCode(503).setBody("""{"error":"Close unavailable"}""")
            else MockResponse().setResponseCode(204)
        }) { vm, requests ->
            compose.runOnUiThread { vm.acceptNode(node) }
            compose.runOnUiThread { vm.closeNode(id) }
            compose.waitUntil(10000) { !vm.state.value.busy && vm.state.value.error == "Close unavailable" }
            assertTrue(vm.state.value.nodes.any { it.number("id") == id })
            compose.runOnUiThread {
                vm.closeNode(id)
                vm.closeNode(id)
                assertTrue(vm.state.value.busy)
                assertTrue(vm.state.value.nodes.any { it.number("id") == id })
            }
            compose.waitUntil(10000) { !vm.state.value.busy && vm.state.value.nodes.none { it.number("id") == id } }
            assertEquals(2, requests.size)
            assertEquals("Agent closed. Worktree and files kept.", vm.state.value.notice)
        }
    }

    @Test fun captureFiltersConfigurationsAndKeepsDraftUntilAnAcknowledgedSuccess() {
        var attempts = 0
        var opened = 0L
        withDesktop({ request ->
            assertEquals("/api/nodes/create", request.path)
            if (++attempts == 1) MockResponse().setResponseCode(503).setBody("""{"error":"Retry this task"}""")
            else MockResponse().setBody(createActive)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                var launch by remember { mutableStateOf("") }
                TaskScreen(state.copy(meshes = listOf(mesh), providers = providers), vm, 3, {}, launch, { launch = it }, { opened = it })
            } }
            compose.onNodeWithText("What should the agent do?").performTextInput("Fix the deadline")
            compose.onNodeWithText("Launch configuration: Mesh default").performClick()
            compose.onNodeWithText("Unavailable agent").assertDoesNotExist()
            compose.onNodeWithText("Saved agent").performClick()
            compose.onNodeWithText("Launch configuration: Saved agent").assertExists()
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
            val bodies = requests.map { Json.parseToJsonElement(it.body.readUtf8()).obj() }
            val keys = bodies.map { it.text("operation_key") }
            assertTrue(keys.all { it.isNotBlank() })
            // #2331: one key per submission, so the retry after a lost response
            // resolves to the same node instead of creating a second one.
            assertEquals(1, keys.distinct().size)
            // The fixture carries the wire shape; the operation key and the
            // typed prompt are this test's own.
            val expected = JsonObject(
                MobileWireFixtures.obj("create_node_request_saved_configuration") +
                    ("operation_key" to JsonPrimitive(keys[0])) +
                    ("prompt" to JsonPrimitive("Fix the deadline"))
            )
            bodies.forEach { assertEquals(expected, it) }
        }
    }

    // The Overview's spawn button opens Capture with a launch already chosen.
    // Mesh default sends no launch fields, and a pre-selected harness that
    // cannot take a prompt is kept and explained rather than swapped.
    @Test fun capturePreselectedLaunchIsKeptAndMeshDefaultLeavesTheChoiceToTheDesktop() {
        var opened = 0L
        withDesktop({ MockResponse().setBody(createActive) }) { vm, requests ->
            val launch = mutableStateOf("terminal")
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                TaskScreen(state.copy(meshes = listOf(mesh), providers = providers), vm, 3, {}, launch.value, { launch.value = it }, { opened = it })
            } }
            compose.onNodeWithText("Launch configuration: Terminal").assertExists()
            compose.onNodeWithText("Start agent").performScrollTo().assertIsEnabled()
            compose.onNodeWithText("What should the agent do?").performTextInput("Fix the deadline")
            compose.onNodeWithText("Terminal can't take an initial prompt", substring = true).assertExists()
            compose.onNodeWithText("Launch configuration: Terminal").assertExists()
            compose.onNodeWithText("Start agent").performScrollTo().assertIsNotEnabled()
            compose.onNodeWithText("Launch configuration: Terminal").performScrollTo().performClick()
            compose.onNodeWithText("Mesh default").performClick()
            compose.onNodeWithText("Start agent").performScrollTo().performClick()
            compose.waitUntil(10000) { opened == 17L && !vm.state.value.busy }
            val body = Json.parseToJsonElement(requests.single().body.readUtf8()).obj()
            assertEquals("/api/nodes/create", requests.single().path)
            val expected = JsonObject(
                MobileWireFixtures.obj("create_node_request_mesh_default") +
                    ("operation_key" to JsonPrimitive(body.text("operation_key"))) +
                    ("prompt" to JsonPrimitive("Fix the deadline"))
            )
            assertEquals(expected, body)
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
            requests.forEach {
                assertEquals(
                    JsonObject(MobileWireFixtures.obj("node_input_request") + ("seq" to JsonPrimitive("Continue safely\r"))),
                    Json.parseToJsonElement(it.body.readUtf8()).jsonObject,
                )
            }
        }
    }

    // Like the desktop Issues probe: the main half starts with the mesh
    // default, the menu lists prompt-capable rows including saved
    // configurations, and either choice starts the agent at once.
    @Test fun issueSplitButtonStartsWithTheMeshDefaultOrAChosenSavedConfiguration() {
        var opened = 0L
        withDesktop({ request ->
            when (request.path) {
                "/api/meshes/3/issues" -> MockResponse().setBody(MobileWireFixtures.raw("issues_feed"))
                "/api/meshes/3/issues/42/spawn" -> MockResponse().setBody(created)
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(meshes = listOf(mesh), providers = providers), vm, { opened = it })
            } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("#42 Fix deadline").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Issue details").assertDoesNotExist()
            // The title, not the merged card: the card's centre is the spawn button.
            compose.onNodeWithText("#42 Fix deadline", useUnmergedTree = true).performClick()
            compose.onNodeWithText("Issue details").assertExists()
            assertEquals(0L, opened)
            compose.onNodeWithContentDescription("Start issue #42 with Mesh default").performClick()
            compose.waitUntil(10000) { opened == 17L && !vm.state.value.busy }
            opened = 0L
            compose.onNodeWithContentDescription("Choose how to start issue #42").performClick()
            compose.onNodeWithText("Terminal").assertDoesNotExist()
            compose.onNodeWithText("Unavailable agent").assertDoesNotExist()
            compose.onNodeWithText("MiniMax via Claude Code").assertExists()
            compose.onNodeWithText("Saved configuration").assertExists()
            compose.onNodeWithText("Saved agent").performClick()
            compose.waitUntil(10000) { opened == 17L && !vm.state.value.busy }
            val posts = requests.filter { it.method == "POST" }
            assertEquals(listOf("/api/meshes/3/issues/42/spawn", "/api/meshes/3/issues/42/spawn"), posts.map { it.path })
            assertEquals(
                listOf(
                    MobileWireFixtures.obj("issue_spawn_request_mesh_default"),
                    MobileWireFixtures.obj("issue_spawn_request_saved_configuration"),
                ),
                posts.map { Json.parseToJsonElement(it.body.readUtf8()).jsonObject },
            )
        }
    }

    // The mesh-wide PR list starts an agent on the PR head; the desktop
    // renders the review prompt, so the body carries only the head fields.
    @Test fun pullRequestSplitButtonStartsAReviewAgentOnTheHead() {
        var opened = 0L
        withDesktop({ request ->
            when (request.path) {
                "/api/meshes/3/pulls?state=open" -> MockResponse().setBody(MobileWireFixtures.raw("pulls_feed"))
                "/api/meshes/3/pulls/42/spawn" -> MockResponse().setBody(created)
                // The panel reads the menu from the ViewModel's own snapshot.
                "/api/meshes" -> MockResponse().setBody(MobileWireFixtures.raw("meshes"))
                "/api/nodes" -> MockResponse().setBody("[]")
                "/api/providers" -> MockResponse().setBody(MobileWireFixtures.raw("providers"))
                else -> MockResponse().setResponseCode(404)
            }
        }) { vm, requests ->
            compose.runOnUiThread { vm.resume() }
            compose.waitUntil(10000) { vm.state.value.providers.isNotEmpty() }
            compose.runOnUiThread { vm.pause() }
            compose.setContent { BuildmeshTheme { PullRequestsScreen(vm.api!!, 3, vm, { opened = it }) } }
            compose.waitUntil(10000) { compose.onAllNodesWithText("PR #42: Fix deadline").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Squash and merge").assertDoesNotExist()
            compose.onNodeWithContentDescription("Choose how to start PR #42").performClick()
            compose.onNodeWithText("Terminal").assertDoesNotExist()
            compose.onNodeWithText("MiniMax via Claude Code").performClick()
            compose.waitUntil(10000) { opened == 17L && !vm.state.value.busy }
            // `resume()` also mints an event-socket ticket, which is a POST too.
            val post = requests.single { it.path?.endsWith("/spawn") == true }
            assertEquals("/api/meshes/3/pulls/42/spawn", post.path)
            assertEquals(
                MobileWireFixtures.obj("pull_spawn_request_proxied"),
                Json.parseToJsonElement(post.body.readUtf8()).jsonObject,
            )
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
            compose.onNodeWithContentDescription("Choose how to start issue", substring = true).assertDoesNotExist()
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
                "/api/meshes/3/issues" -> MockResponse().setBody(
                    JsonObject(
                        MobileWireFixtures.obj("issues_feed") +
                            ("items" to JsonArray(listOf(
                                JsonObject(
                                    issueRow +
                                        ("number" to JsonPrimitive(42)) +
                                        ("title" to JsonPrimitive("Fix the mobile deadline and preserve a long issue title on a narrow phone screen")) +
                                        ("body" to JsonPrimitive("Expanded issue details")) +
                                        ("url" to JsonPrimitive("https://github.com/o/r/issues/42")) +
                                        ("labels" to JsonArray(if (tagged) listOf(JsonPrimitive("bug"), JsonPrimitive("buildmesh:run")) else listOf(JsonPrimitive("bug")))) +
                                        ("blocked_by" to JsonArray(listOf(JsonPrimitive(43), JsonPrimitive(99)))) +
                                        ("author" to JsonPrimitive("octo"))
                                ),
                                JsonObject(
                                    issueRow +
                                        ("number" to JsonPrimitive(43)) +
                                        ("title" to JsonPrimitive("Prerequisite")) +
                                        ("body" to JsonPrimitive("")) +
                                        ("url" to JsonPrimitive("")) +
                                        ("labels" to JsonArray(emptyList())) +
                                        ("blocked_by" to JsonArray(emptyList())) +
                                        ("author" to JsonPrimitive(""))
                                ),
                            ))) +
                            ("completeness" to triageCompleteness)
                    ).toString()
                )
                "/api/meshes/3/issue-labels" -> MockResponse().setBody(MobileWireFixtures.raw("issue_labels"))
                "/api/meshes/3/issues/42/label" -> {
                    if (++attempts == 1) MockResponse().setResponseCode(500).setBody("""{"error":"GitHub permission denied"}""")
                    else {
                        val body = Json.parseToJsonElement(request.body.clone().readUtf8()).jsonObject
                        // The label the phone writes carries the canonical
                        // request's key set; the label and the toggle are this
                        // test's own.
                        assertEquals(MobileWireFixtures.obj("issue_label_request").keys, body.keys)
                        assertEquals("buildmesh:run", body.text("label"))
                        tagged = body.flag("present")
                        MockResponse().setResponseCode(204)
                    }
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
            compose.onNodeWithText("#42 Fix the mobile", substring = true, useUnmergedTree = true).performClick()
            compose.onNodeWithText("Expanded issue details").assertExists()
            compose.onNodeWithContentDescription("Choose how to start issue #42").performScrollTo().performClick()
            compose.onNodeWithText("MiniMax via Claude Code").assertExists()
            issueScreenshot("issues-android-launch")
            androidx.test.espresso.Espresso.pressBack()
            compose.onNodeWithText("MiniMax via Claude Code").assertDoesNotExist()
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
                "/api/meshes/3/agent-nodes/discover" -> MockResponse().setBody(MobileWireFixtures.raw("archived_agent_nodes"))
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
            compose.onNodeWithText("Launch configuration: MiniMax via Claude Code").assertExists()
            compose.onNodeWithText("Start").performClick()
            compose.waitUntil(10000) { vm.state.value.error.contains("Node 17 was imported") && !vm.state.value.busy }
            compose.onNodeWithText("Choose a launch configuration").assertDoesNotExist()
            assertEquals(0L, opened)
            assertTrue(vm.state.value.error.contains("Executable missing"))
            val request = requests.single { it.method == "POST" }
            // The provider is the Spawn Option the archive dialog preselected.
            val expected = MobileWireFixtures.obj("import_and_resume_request")
            assertEquals(archiveProvider, expected.text("provider"))
            assertEquals(expected, Json.parseToJsonElement(request.body.readUtf8()).jsonObject)
        }
    }

    @Test fun pullRequestChecksDesktopAuthenticationAndKeepsDraftUntilSuccess() {
        var checks = 0
        withDesktop({ request ->
            when (request.path) {
                // The desktop has no GitHub session yet: that is this test's
                // own scenario, not a wire shape.
                "/api/gh/auth" -> MockResponse().setBody(if (++checks == 1) """{"ok":false}""" else MobileWireFixtures.raw("gh_auth"))
                "/api/meshes/3/pr/source?node_id=17" -> MockResponse().setBody(MobileWireFixtures.raw("pr_source"))
                "/api/meshes/3/pr" -> MockResponse().setBody(MobileWireFixtures.raw("create_pr_result"))
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
            // The head branch is the one the previewed source reported.
            val expected = JsonObject(
                MobileWireFixtures.obj("create_pr_request") +
                    ("head_branch" to JsonPrimitive(MobileWireFixtures.obj("pr_source").text("head_branch")))
            )
            assertEquals(expected, Json.parseToJsonElement(request.body.readUtf8()).jsonObject)
        }
    }

    /** The canonical pull feed, with this test's own rows and completeness. */
    private fun pullResponse(items: List<JsonObject>, complete: Boolean = true) = JsonObject(
        MobileWireFixtures.obj("pulls_feed") +
            ("items" to JsonArray(items)) +
            ("completeness" to JsonObject(
                MobileWireFixtures.obj("pulls_feed").objectAt("completeness")!!.mapValues { (key, value) ->
                    when (key) {
                        "complete" -> JsonPrimitive(complete)
                        "incomplete_reason" -> if (complete) JsonNull else JsonPrimitive("safety_cap")
                        else -> value
                    }
                },
            ))
    ).toString()

    /** The open pull request this test merges, with its own row values. */
    private val openPull = JsonObject(
        pullRow +
            ("title" to JsonPrimitive("Fix search")) +
            ("url" to JsonPrimitive("https://github.com/o/r/pull/42")) +
            ("head_ref" to JsonPrimitive("feature/search")) +
            ("head_repo_owner" to JsonPrimitive("o")) +
            ("head_repo_clone_url" to JsonPrimitive("https://github.com/o/r.git")) +
            ("head_sha" to JsonPrimitive("abc")) +
            ("author" to JsonPrimitive("o")) +
            ("mergeable" to JsonPrimitive(true)) +
            ("mergeable_state" to JsonPrimitive("clean")) +
            ("checks_state" to JsonPrimitive("success")) +
            ("review_decision" to JsonPrimitive("approved"))
    )

    @Test fun aNodePullShowsStatusConfirmsSquashAndRemovesAnAcknowledgedMerge() {
        var merged = false
        withDesktop({ request -> when (request.path) {
            "/api/agents/17/git/branch" -> MockResponse().setBody(JsonObject(MobileWireFixtures.obj("git_branch") + ("branch" to JsonPrimitive("feature/search"))).toString())
            "/api/meshes/3/pulls?state=open" -> MockResponse().setBody(pullResponse(if (merged) emptyList() else listOf(openPull)))
            "/api/meshes/3/pulls/42/mergeability" -> MockResponse().setBody(MobileWireFixtures.raw("pr_mergeability"))
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
            "/api/agents/17/git/branch" -> MockResponse().setBody(JsonObject(MobileWireFixtures.obj("git_branch") + ("branch" to JsonPrimitive("feature/search"))).toString())
            "/api/meshes/3/pulls?state=open" -> MockResponse().setBody(
                pullResponse(listOf(if (blocked) JsonObject(openPull + ("mergeable_state" to JsonPrimitive("blocked"))) else openPull), false))
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
