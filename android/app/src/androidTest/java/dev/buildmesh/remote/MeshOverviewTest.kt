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
                Dashboard(RemoteState(meshes = meshes, nodes = nodes), "Overview", 0, {}, {}, { _, _ -> })
            }
        } }
        compose.onNodeWithText("/src/empty").performScrollTo().assertIsDisplayed()
        saveScreenshot("mesh-overview")
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
                compose.onNodeWithText("Website agent").performScrollTo().performClick()
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

    @Test fun summariesTrackOpenNodesAndKeepEmptyMeshesUsable() {
        val state = mutableStateOf(RemoteState(meshes = meshes, nodes = nodes))
        var opened = 0L
        var target = ""
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state.value, "Overview", 0, {}, {}, { id, screen -> opened = id; target = screen })
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
                Dashboard(RemoteState(meshes = listOf(meshes[1]), nodes = manyNodes), "Overview", 0, {}, {}, { _, _ -> })
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
        compose.onNodeWithText("Archive").performScrollTo().assertIsDisplayed()
        saveScreenshot("mesh-many-dots")
    }
}
