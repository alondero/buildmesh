package dev.buildmesh.remote

import android.graphics.Bitmap
import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.material3.Surface
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.toPixelMap
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.core.app.ApplicationProvider
import androidx.test.espresso.Espresso.closeSoftKeyboard
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
                Dashboard(RemoteState(meshes = meshes, nodes = nodes), "Overview", 0, {}, {}, { _, _ -> }, { _, _ -> })
            }
        } }
        compose.onNodeWithText("Empty mesh").performScrollTo().assertIsDisplayed()
        meshes.forEach { compose.onNodeWithText(it.text("path")).assertDoesNotExist() }
        listOf("Issues", "Pull requests", "Archive").forEach { compose.onNodeWithText(it).assertDoesNotExist() }
        val title = compose.onNodeWithText("Website with a long mesh name").fetchSemanticsNode().boundsInRoot
        val spawn = compose.onNodeWithContentDescription("Start a task in Website with a long mesh name with Mesh default").fetchSemanticsNode().boundsInRoot
        val menu = compose.onNodeWithContentDescription("Choose how to start a task in Website with a long mesh name").fetchSemanticsNode().boundsInRoot
        assertTrue("Spawn sits beside the name", spawn.left >= title.right && spawn.top == title.top)
        assertTrue("Menu fits beside Spawn", menu.left >= spawn.right && menu.right <= compose.onRoot().fetchSemanticsNode().boundsInRoot.right)
        assertTrue("Spawn keeps a 48dp touch target", spawn.width >= 48 * compose.density.density && spawn.height >= 48 * compose.density.density)
        assertTrue("Menu keeps a 48dp touch target", menu.width >= 48 * compose.density.density && menu.height >= 48 * compose.density.density)
        saveScreenshot("mesh-overview")
    }

    @Test fun workResourcesRemainAvailableForEmptyAndFilteredMeshes() {
        val state = mutableStateOf(RemoteState(meshes = meshes, nodes = nodes))
        val selected = mutableStateOf(0L)
        val opened = mutableListOf<Pair<Long, String>>()
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state.value, "Work", selected.value, { selected.value = it }, {},
                    { id, target -> opened += id to target }, { _, _ -> })
            }
        } }
        // All meshes includes resource links for the empty mesh, not just agent groups.
        compose.onNodeWithText("Empty mesh", substring = false).performScrollTo().assertIsDisplayed()
        compose.runOnIdle { selected.value = 3L }
        compose.onNodeWithText("Pull requests").performScrollTo().performClick()
        compose.onNodeWithText("Issues", substring = false).performClick()
        compose.onNodeWithText("Archive").performClick()
        compose.onNodeWithText("No agents match this view. Capture a task to start one.").assertIsDisplayed()
        compose.runOnIdle { selected.value = 1L }
        compose.onNodeWithText("Search agents or branches").performScrollTo().performTextInput("no matches")
        closeSoftKeyboard()
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
                Dashboard(state.value, "Overview", 3, {}, {}, { _, _ -> }, { _, _ -> })
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
                compose.onNodeWithText("Search agents or branches").performTextInput("no matching agent")
                closeSoftKeyboard()
                compose.onNodeWithText("Needs attention", substring = false).performClick()
                compose.onNodeWithText("Buildmesh", substring = false).performScrollTo().performClick()
                compose.onNodeWithText("Your work").assertIsDisplayed()
                compose.onNodeWithText("Mesh: Buildmesh").assertIsDisplayed()
                compose.onNodeWithText("no matching agent").assertDoesNotExist()
                compose.onNode(hasText("Needs attention") and SemanticsMatcher.keyIsDefined(SemanticsProperties.Selected)).assertIsNotSelected()
                compose.onNodeWithText("Buildmesh agent").performScrollTo().assertIsDisplayed()
                compose.onNodeWithText("Waiting agent").performScrollTo().assertIsDisplayed()
                compose.onNodeWithText("Website agent").assertDoesNotExist()
                saveScreenshot("mesh-work")

                compose.onNodeWithText("Mesh: Buildmesh").performScrollTo().performClick()
                compose.onNodeWithText("All meshes", substring = false).performClick()
                compose.onNode(hasScrollToIndexAction()).performScrollToNode(hasText("Website agent"))
                compose.onNodeWithText("Website agent").performClick()
                compose.onNodeWithText("Open terminal").assertIsDisplayed()
                compose.onNodeWithContentDescription("Back").performClick()
                compose.onNodeWithText("Overview", substring = false).performClick()
                compose.onNode(hasScrollToIndexAction()).performScrollToNode(hasText("Empty mesh"))
                compose.onNodeWithText("Empty mesh").performClick()
                compose.onNodeWithText("Your work").assertIsDisplayed()
                compose.onNodeWithText("Mesh: Empty mesh").assertIsDisplayed()
                compose.onNodeWithText("No agents match this view. Capture a task to start one.").assertIsDisplayed()
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
                Dashboard(RemoteState(meshes = listOf(meshes[0], pinned), nodes = nodes, providers = providers), "Overview", 0, {}, {}, { _, _ -> },
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
                Dashboard(state.value, "Overview", 0, {}, {}, { id, screen -> opened = id; target = screen }, { _, _ -> })
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

    @Test fun manyDotsWrapInsideTheMeshCard() {
        val manyNodes = (1..150).map { id ->
            Json.parseToJsonElement("""{"id":$id,"mesh_id":2,"name":"Agent $id","status":"running"}""").obj()
        }
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = manyNodes), "Overview", 0, {}, {}, { _, _ -> }, { _, _ -> })
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
