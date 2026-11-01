package dev.buildmesh.remote

import android.graphics.Bitmap
import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.material3.Surface
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.toPixelMap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.core.app.ApplicationProvider
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class MeshOverviewTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val meshes = listOf(
        """{"id":1,"name":"Buildmesh","path":"/src/buildmesh"}""",
        """{"id":2,"name":"Website with a long mesh name","path":"/src/website"}""",
        """{"id":3,"name":"Empty mesh","path":"/src/empty"}""",
    ).map { Json.parseToJsonElement(it).obj() }
    private val nodes = listOf(
        """{"id":11,"mesh_id":1,"name":"Buildmesh agent","status":"running"}""",
        """{"id":12,"mesh_id":1,"name":"Waiting agent","status":"awaiting_input"}""",
        """{"id":21,"mesh_id":2,"name":"Website agent","status":"ready"}""",
    ).map { Json.parseToJsonElement(it).obj() }

    private fun saveScreenshot(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val screenshot = compose.onRoot().captureToImage().asAndroidBitmap()
        try {
            instrumentation.targetContext.cacheDir.resolve("$name.png").outputStream().use {
                assertTrue(screenshot.compress(Bitmap.CompressFormat.PNG, 100, it))
            }
        } finally { screenshot.recycle() }
    }

    @Test fun overviewScreenshot() {
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = meshes, nodes = nodes), HomePane.Meshes, 0, {}, {}, { _, _ -> }, { _, _ -> })
            }
        } }
        compose.onNodeWithText("Empty mesh").performScrollTo().assertIsDisplayed()
        meshes.forEach { compose.onNodeWithText(it.text("path")).assertDoesNotExist() }
        listOf("Issues", "Pull requests", "Archive", "Buildmesh agent", "Waiting agent", "Website agent").forEach {
            compose.onNodeWithText(it).assertDoesNotExist()
        }
        compose.onNodeWithText("All meshes").assertDoesNotExist()
        compose.onNodeWithText("Search agents or branches").assertDoesNotExist()
        val title = compose.onNodeWithText("Website with a long mesh name").fetchSemanticsNode().boundsInRoot
        val spawn = compose.onNodeWithContentDescription("Start a task in Website with a long mesh name with Mesh default").fetchSemanticsNode().boundsInRoot
        val menu = compose.onNodeWithContentDescription("Choose how to start a task in Website with a long mesh name").fetchSemanticsNode().boundsInRoot
        assertTrue("Spawn sits beside the name", spawn.left >= title.right && spawn.top == title.top)
        assertTrue("Menu fits beside Spawn", menu.left >= spawn.right && menu.right <= compose.onRoot().fetchSemanticsNode().boundsInRoot.right)
        assertTrue("Spawn keeps a 48dp touch target", spawn.width >= 48 * compose.density.density && spawn.height >= 48 * compose.density.density)
        assertTrue("Menu keeps a 48dp touch target", menu.width >= 48 * compose.density.density && menu.height >= 48 * compose.density.density)
        saveScreenshot("mesh-overview")
    }

    @Test fun workResourcesRemainAvailableForAnEmptyMesh() {
        val state = mutableStateOf(RemoteState(meshes = meshes, nodes = nodes))
        val selected = mutableStateOf(3L)
        val opened = mutableListOf<Pair<Long, String>>()
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state.value, HomePane.Work, selected.value, { selected.value = it }, {},
                    { id, target -> opened += id to target }, { _, _ -> })
            }
        } }
        compose.onNodeWithText("Pull requests").performScrollTo().performClick()
        compose.onNodeWithText("Issues", substring = false).performClick()
        compose.onNodeWithText("Archive").performClick()
        compose.onNodeWithText("No agents in this mesh yet.").assertIsDisplayed()
        compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
        compose.runOnIdle { selected.value = 1L }
        compose.onNodeWithText("Buildmesh agent").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Website agent").assertDoesNotExist()
        compose.onNodeWithText("Pull requests").performScrollTo().performClick()
        compose.onNodeWithText("Issues", substring = false).performClick()
        compose.onNodeWithText("Archive").performClick()
        compose.runOnIdle { assertEquals(listOf(3L to "pulls", 3L to "issues", 3L to "archive", 1L to "pulls", 1L to "issues", 1L to "archive"), opened) }
        saveScreenshot("mesh-work-resources")
    }

    @Test fun overviewUsesDesktopBandsAndReordersAfterArchiveAndClose() {
        // Non-alphabetical saved order; archived-only meshes belong in the quiet band.
        val savedOrder = listOf(meshes[2], meshes[1], meshes[0])
        val archived = Json.parseToJsonElement("""{"id":31,"mesh_id":3,"name":"Archived agent","status":"archived"}""").obj()
        val state = mutableStateOf(RemoteState(meshes = savedOrder, nodes = nodes + archived))
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state.value, HomePane.Meshes, 3, {}, {}, { _, _ -> }, { _, _ -> })
            }
        } }
        fun assertOrder(names: List<String>) {
            compose.onNode(hasScrollToIndexAction()).performScrollToIndex(2)
            val tops = names.map { compose.onNodeWithText(it, substring = false).fetchSemanticsNode().boundsInRoot.top }
            assertTrue("Expected mesh order: $names", tops.zipWithNext().all { (a, b) -> a < b })
        }
        assertOrder(listOf("Website with a long mesh name", "Buildmesh", "Empty mesh"))
        compose.runOnIdle {
            state.value = state.value.copy(nodes = listOf(nodes[0], nodes[1], archived,
                Json.parseToJsonElement("""{"id":21,"mesh_id":2,"name":"Website agent","status":"archived"}""").obj()))
        }
        assertOrder(listOf("Buildmesh", "Empty mesh", "Website with a long mesh name"))
        compose.runOnIdle { state.value = state.value.copy(nodes = listOf(archived)) }
        assertOrder(listOf("Empty mesh", "Website with a long mesh name", "Buildmesh"))
    }

    @Test fun meshNameOpensWorkByIdAndClearsStaleFilters() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = when (request.path) {
                    "/api/session" -> MockResponse().setResponseCode(204)
                    "/api/meshes" -> MockResponse().setBody(meshes.toString())
                    "/api/nodes" -> MockResponse().setBody(nodes.toString())
                    else -> MockResponse().setBody("[]")
                }
            }
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            lateinit var vm: BuildmeshViewModel
            compose.runOnUiThread { vm = BuildmeshViewModel(app) }
            try {
                compose.setContent { BuildmeshTheme { BuildmeshApp(vm) } }
                compose.waitUntil(10000) { !vm.state.value.restoring }
                compose.runOnUiThread { vm.resume() }
                compose.waitUntil(10000) { vm.state.value.meshes.size == 3 }
                compose.runOnUiThread { vm.pause() }
                compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
                compose.onNodeWithText("All meshes").assertDoesNotExist()
                compose.onNodeWithText("Search agents or branches").assertDoesNotExist()
                compose.onNodeWithText("Buildmesh").performScrollTo().performClick()
                compose.onNodeWithText("Buildmesh agent").performScrollTo().assertIsDisplayed()
                compose.onNodeWithText("Waiting agent").performScrollTo().assertIsDisplayed()
                compose.onNodeWithText("Website agent").assertDoesNotExist()
                compose.onNodeWithText("All meshes").assertDoesNotExist()
                saveScreenshot("mesh-work")

                compose.onNodeWithText("Buildmesh agent").performClick()
                compose.onNodeWithText("Open terminal").assertIsDisplayed()
                // Back from an agent lands on the mesh it belongs to, and Back
                // from the mesh page returns to the mesh list.
                compose.onNodeWithContentDescription("Back").performClick()
                compose.onNodeWithText("Buildmesh agent").assertIsDisplayed()
                compose.onNodeWithContentDescription("Back").performClick()
                compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
                compose.onNode(hasScrollToIndexAction()).performScrollToNode(hasText("Empty mesh"))
                compose.onNodeWithText("Empty mesh").performClick()
                compose.onNodeWithText("No agents in this mesh yet.").assertIsDisplayed()
                compose.onNodeWithText("Website agent").assertDoesNotExist()
            } finally {
                compose.runOnUiThread { vm.forget() }
                store.clear()
            }
        }
    }

    private val providers = Json.parseToJsonElement("""[
        {"id":"terminal","label":"Terminal","resumable":false,"capabilities":{"supports_prefill":false}},
        {"id":"launch/saved","label":"Saved agent","resumable":true,"capabilities":{"supports_prefill":true},"configuration":{"id":"launch/saved","spawn_option_id":"claude","name":"Saved agent"}},
        {"id":"claude","label":"Claude Code","resumable":true,"capabilities":{"supports_prefill":true}},
        {"id":"missing","label":"Unavailable agent","unavailable_reason":"Missing executable","resumable":true,"capabilities":{"supports_prefill":true}}
    ]""").objects()

    // Both halves of a mesh's spawn button open Capture rather than spawning:
    // Capture is where the first prompt is written.
    @Test fun meshSpawnButtonOpensCaptureWithTheMeshAndChosenLaunch() {
        val pinned = Json.parseToJsonElement("""{"id":4,"name":"Pinned","path":"/src/pinned","default_provider":"claude"}""").obj()
        val opened = mutableListOf<Pair<Long, String>>()
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[0], pinned), nodes = nodes, providers = providers), HomePane.Meshes, 0, {}, {}, { _, _ -> },
                    { id, option -> opened += id to option })
            }
        } }
        compose.onNodeWithContentDescription("Start a task in Buildmesh with Mesh default").performClick()
        compose.onNodeWithContentDescription("Choose how to start a task in Buildmesh").performClick()
        compose.onNodeWithText("Unavailable agent").assertDoesNotExist()
        compose.onNodeWithText("Terminal").performClick()
        compose.onNodeWithContentDescription("Start a task in Pinned with Claude Code").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(listOf(1L to "", 1L to "terminal", 4L to ""), opened) }
    }

    @Test fun theOverviewSpawnMenuLandsOnCapturePrefilled() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = when (request.path) {
                    "/api/session" -> MockResponse().setResponseCode(204)
                    "/api/meshes" -> MockResponse().setBody(meshes.toString())
                    "/api/nodes" -> MockResponse().setBody(nodes.toString())
                    "/api/providers" -> MockResponse().setBody(providers.toString())
                    else -> MockResponse().setBody("[]")
                }
            }
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            lateinit var vm: BuildmeshViewModel
            compose.runOnUiThread { vm = BuildmeshViewModel(app) }
            try {
                compose.setContent { BuildmeshTheme { BuildmeshApp(vm) } }
                compose.waitUntil(10000) { !vm.state.value.restoring }
                compose.runOnUiThread { vm.resume() }
                compose.waitUntil(10000) { vm.state.value.meshes.size == 3 && vm.state.value.providers.isNotEmpty() }
                compose.runOnUiThread { vm.pause() }
                compose.onNodeWithContentDescription("Choose how to start a task in Website with a long mesh name").performScrollTo().performClick()
                compose.onNodeWithText("Saved agent").performClick()
                compose.onNodeWithText("New task").assertIsDisplayed()
                compose.onNodeWithText("Mesh: Website with a long mesh name").assertIsDisplayed()
                compose.onNodeWithText("Launch configuration: Saved agent").assertIsDisplayed()
                saveScreenshot("capture-prefilled")
                compose.onNodeWithContentDescription("Back").performClick()
                compose.onNodeWithContentDescription("Start a task in Buildmesh with Mesh default").performScrollTo().performClick()
                compose.onNodeWithText("Mesh: Buildmesh").assertIsDisplayed()
                compose.onNodeWithText("Launch configuration: Mesh default").assertIsDisplayed()
            } finally {
                compose.runOnUiThread { vm.forget() }
                store.clear()
            }
        }
    }

    @Test fun summariesTrackOpenNodesAndKeepEmptyMeshesUsable() {
        val state = mutableStateOf(RemoteState(meshes = meshes, nodes = nodes))
        var opened = 0L
        var target = ""
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state.value, HomePane.Meshes, 0, {}, {}, { id, screen -> opened = id; target = screen }, { _, _ -> })
            }
        } }
        compose.onNodeWithContentDescription("2 agents: Running, Needs attention", useUnmergedTree = true).assertExists()
        compose.onNodeWithText("Empty mesh").performScrollTo().assertHasClickAction()
        val pixels = compose.onNodeWithText("Empty mesh", useUnmergedTree = true).captureToImage().toPixelMap()
        assertTrue("Empty mesh name uses the muted palette token", (0 until pixels.height).any { y ->
            (0 until pixels.width).any { x -> pixels[x, y].toArgb() == MeshColors.muted.toArgb() }
        })
        compose.onNodeWithText("Empty mesh").performClick()
        compose.runOnIdle { assertEquals(3L, opened); assertEquals("work", target) }
        compose.runOnIdle {
            state.value = state.value.copy(nodes = listOf(
                nodes[0].toMutableMap().also { it["status"] = kotlinx.serialization.json.JsonPrimitive("archived") }.let { kotlinx.serialization.json.JsonObject(it) },
                nodes[1], nodes[2],
            ))
        }
        compose.onNodeWithText("Buildmesh", substring = false).performScrollTo()
        compose.onNodeWithContentDescription("1 agent: Needs attention", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("2 agents: Running, Needs attention", useUnmergedTree = true).assertDoesNotExist()
    }

    private val waitingPermission = Json.parseToJsonElement("""
        {"id":12,"mesh_id":2,"name":"Waiting agent","branch":"task","status":"awaiting_input",
         "lifecycle":{"kind":"permission_requested","status":"awaiting_input","timestamp":"2026-10-26T10:00:00Z",
                      "message":"Run the failing test?",
                      "semantic_turn":{"kind":"permission_request","description":"Run the failing test?"},
                      "request":{"choices":[]}}}
    """).obj()
    private val waitingQuestion = Json.parseToJsonElement("""
        {"id":13,"mesh_id":2,"name":"Asking agent","branch":"task","status":"awaiting_input",
         "lifecycle":{"kind":"question_requested","status":"awaiting_input","timestamp":"2026-10-26T10:05:00Z",
                      "message":"Deploy where?",
                      "request":{"choices":["Staging","Production"]}}}
    """).obj()

    // Work for the selected mesh pins whoever is blocked on you, and the
    // approval pair sends the same keystrokes the desktop semantic-turn banner
    // and the browser deck send. The pinned copy is the only copy.
    @Test fun waitingAgentsArePinnedWithOneTapApproval() {
        var answered: Pair<Long, String>? = null
        var opened = 0L
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = listOf(waitingPermission)), HomePane.Work, 2, {}, { opened = it }, { _, _ -> }, { _, _ -> })
                { id, sequence, onDone -> answered = id to sequence; onDone(true); true }
            }
        } }
        compose.onNodeWithText("Waiting for input").assertIsDisplayed()
        compose.onAllNodesWithText("Run the failing test?").assertCountEquals(1)
        compose.onAllNodesWithText("Waiting agent").assertCountEquals(1)
        compose.onAllNodesWithText("Website with a long mesh name · task").assertCountEquals(1)
        compose.onNodeWithText("Allow (Y)").performClick()
        compose.runOnIdle { assertEquals(12L to "y\r", answered) }
        compose.onNodeWithText("Approved ✓").assertIsDisplayed()
        compose.onNodeWithText("Allow (Y)").assertDoesNotExist()
        compose.onNodeWithText("Reject (N)").assertIsNotEnabled()
        saveScreenshot("attention-deck")
    }

    // A reply the transport never took must hand the controls back: the deck's
    // in-flight marker is cleared by the failure signal, not left showing
    // "Sending…" until the desktop replaces the request.
    @Test fun aReplyThatNeverReachedTheDesktopReEnablesTheApprovalPair() {
        var answered: Pair<Long, String>? = null
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = listOf(waitingPermission)), HomePane.Work, 2, {}, {}, { _, _ -> }, { _, _ -> })
                { id, sequence, onDone -> answered = id to sequence; onDone(false); true }
            }
        } }
        compose.onNodeWithText("Allow (Y)").performClick()
        compose.runOnIdle { assertEquals(12L to "y\r", answered) }
        compose.onNodeWithText("Sending…").assertDoesNotExist()
        compose.onNodeWithText("Allow (Y)").assertIsEnabled()
        compose.onNodeWithText("Reject (N)").assertIsEnabled()
    }

    // A question has no keystroke answer, so the deck offers the terminal instead
    // of guessing how the harness selects one of its own options — and the chip
    // really opens the terminal, not the node detail behind its label.
    @Test fun aQuestionOffersTheTerminalRatherThanAKeystroke() {
        var opened = 0L
        var terminal = 0L
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = listOf(waitingQuestion)), HomePane.Work, 2, {}, { opened = it }, { _, _ -> }, { _, _ -> },
                    openTerminal = { terminal = it }) { _, _, _ -> true }
            }
        } }
        compose.onNodeWithText("Deploy where?").assertIsDisplayed()
        compose.onNodeWithText("• Staging").assertIsDisplayed()
        compose.onNodeWithText("Allow (Y)").assertDoesNotExist()
        compose.onNodeWithText("Answer in terminal").performClick()
        compose.runOnIdle { assertEquals(13L, terminal); assertEquals(0L, opened) }
    }

    // The deck is the only copy of a waiting agent. The rows under it do not
    // repeat that agent.
    @Test fun theDeckDoesNotDuplicateTheWorkTabRows() {
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = listOf(waitingPermission)), HomePane.Work, 2, {}, {}, { _, _ -> }, { _, _ -> }) { _, _, _ -> true }
            }
        } }
        compose.onNodeWithText("Waiting for input").assertIsDisplayed()
        compose.onAllNodesWithText("Waiting agent").assertCountEquals(1)
    }

    @Test fun splitDashboardShowsTheMeshListBesideTheSelectedMesh() {
        val selected = mutableStateOf(0L)
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = meshes, nodes = nodes), HomePane.Meshes, selected.value,
                    { selected.value = it }, {},
                    { id, target -> if (target == "work") selected.value = id },
                    { _, _ -> }, split = true)
            }
        } }
        compose.onNodeWithText("Empty mesh").assertIsDisplayed()
        compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
        compose.onNodeWithText("All meshes").assertDoesNotExist()
        compose.onNodeWithText("Buildmesh").performClick()
        compose.onNodeWithText("Empty mesh").assertIsDisplayed()
        compose.onNodeWithText("Buildmesh agent").assertIsDisplayed()
        compose.onNodeWithText("Website agent").assertDoesNotExist()
    }

    @Test fun manyDotsWrapInsideTheMeshCard() {
        val manyNodes = (1..150).map { id ->
            Json.parseToJsonElement("""{"id":$id,"mesh_id":2,"name":"Agent $id","status":"running"}""").obj()
        }
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = manyNodes), HomePane.Meshes, 0, {}, {}, { _, _ -> }, { _, _ -> })
            }
        } }
        val title = compose.onNodeWithText("Website with a long mesh name").performScrollTo().assertHasClickAction()
        val summary = compose.onNodeWithContentDescription("150 agents: " + List(150) { "Running" }.joinToString(", "), useUnmergedTree = true)
        summary.assertIsDisplayed()
        val titleBounds = title.fetchSemanticsNode().boundsInRoot
        val summaryBounds = summary.fetchSemanticsNode().boundsInRoot
        assertTrue("Dots wrap to multiple lines", summaryBounds.height > 20 * compose.density.density)
        assertTrue(summaryBounds.left >= titleBounds.left)
        assertTrue(summaryBounds.right <= titleBounds.right)
        compose.onNodeWithText("150", useUnmergedTree = true).assertIsDisplayed()
        val menu = compose.onNodeWithContentDescription("Choose how to start a task in Website with a long mesh name").assertIsDisplayed()
        assertEquals("Spawn remains beside the name when dots wrap", titleBounds.top, menu.fetchSemanticsNode().boundsInRoot.top)
        saveScreenshot("mesh-many-dots")
    }
}
