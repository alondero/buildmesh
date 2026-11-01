package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.unit.Density
import androidx.test.core.app.ApplicationProvider
import androidx.test.espresso.Espresso
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Rule
import org.junit.Test

/**
 * The app's navigation structure, driven through the real shell: peer
 * destinations in a bar (a rail on a wide window), a mesh page one step below
 * Meshes, and "New task" as an action rather than a tab.
 */
class AppNavigationTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val meshes = listOf(
        """{"id":1,"name":"Buildmesh","path":"/src/buildmesh"}""",
        """{"id":2,"name":"Website","path":"/src/website"}""",
    ).map { Json.parseToJsonElement(it).obj() }
    private val nodes = listOf(
        """{"id":11,"mesh_id":1,"name":"Buildmesh agent","status":"running"}""",
        """{"id":21,"mesh_id":2,"name":"Website agent","status":"ready"}""",
    ).map { Json.parseToJsonElement(it).obj() }

    /** Pairs the shell with a mock desktop, waits for the inventory, and runs [block]. */
    private fun withApp(density: Float? = null, block: () -> Unit) {
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
                compose.setContent { BuildmeshTheme {
                    if (density == null) BuildmeshApp(vm)
                    else CompositionLocalProvider(LocalDensity provides Density(density, LocalDensity.current.fontScale)) { BuildmeshApp(vm) }
                } }
                compose.waitUntil(10000) { !vm.state.value.restoring }
                compose.runOnUiThread { vm.resume() }
                compose.waitUntil(10000) { vm.state.value.meshes.size == 2 && vm.state.value.nodes.size == 2 }
                compose.runOnUiThread { vm.pause() }
                block()
            } finally {
                compose.runOnUiThread { vm.forget() }
                store.clear()
            }
        }
    }

    // Whole-screen capture, so the system bars and the real device are in the
    // picture; it is evidence for a human to look at, not an assertion.
    private fun saveScreenshot(name: String) {
        val instrumentation = androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
        val image = instrumentation.uiAutomation.takeScreenshot()
        instrumentation.targetContext.cacheDir.resolve("$name.png").outputStream().use {
            image.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }

    // Lowering the density makes the phone window measure 720dp, so the 600dp
    // wide layout runs inside the real window.
    private fun wideDensity() = compose.activity.resources.displayMetrics.widthPixels / 720f

    @Test fun aPhoneOffersThreePeerDestinationsAndNoTabsThatDoNothing() {
        withApp {
            compose.onNodeWithTag("bottom-navigation").assertIsDisplayed()
            compose.onNodeWithTag("navigation-rail").assertDoesNotExist()
            listOf("Meshes", "Usage", "Settings").forEach {
                compose.onNode(hasText(it) and hasAnyAncestor(hasTestTag("bottom-navigation"))).assertIsDisplayed()
            }
            // Overview/Work were two halves of one list-then-detail path, and
            // Capture was an action: none of them is a destination.
            listOf("Overview", "Work", "Capture").forEach { compose.onNodeWithText(it).assertDoesNotExist() }
            compose.onNode(hasText("Meshes") and hasAnyAncestor(hasTestTag("bottom-navigation"))).assertIsSelected()
            saveScreenshot("navigation-phone")
        }
    }

    @Test fun aMeshIsOneStepBelowMeshesAndBackClimbsOneStep() {
        withApp {
            compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
            compose.onNodeWithText("Buildmesh").performClick()
            compose.onNodeWithText("Buildmesh agent").assertIsDisplayed()
            compose.onNodeWithText("Website agent").assertDoesNotExist()
            // The mesh page stays inside the Meshes destination and keeps the bar.
            compose.onNode(hasText("Meshes") and hasAnyAncestor(hasTestTag("bottom-navigation"))).assertIsSelected()
            compose.onNodeWithText("Buildmesh agent").performClick()
            compose.onNodeWithText("Open terminal").assertIsDisplayed()
            compose.onNodeWithTag("bottom-navigation").assertDoesNotExist()
            compose.onNodeWithContentDescription("Back").performClick()
            compose.onNodeWithText("Buildmesh agent").assertIsDisplayed()
            compose.onNodeWithTag("bottom-navigation").assertIsDisplayed()
            compose.onNodeWithContentDescription("Back").performClick()
            compose.onNodeWithText("Website").assertIsDisplayed()
            compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
        }
    }

    @Test fun reselectingMeshesFromAMeshReturnsToTheList() {
        withApp {
            compose.onNodeWithText("Website").performClick()
            compose.onNodeWithText("Website agent").assertIsDisplayed()
            compose.onNode(hasText("Meshes") and hasAnyAncestor(hasTestTag("bottom-navigation"))).performClick()
            compose.onNodeWithText("Website agent").assertDoesNotExist()
            compose.onNodeWithText("Buildmesh").assertIsDisplayed()
        }
    }

    @Test fun usageAndSettingsAreDestinationsWhoseBackIsMeshes() {
        withApp {
            val bar = hasAnyAncestor(hasTestTag("bottom-navigation"))
            compose.onNode(hasText("Settings") and bar).performClick()
            compose.onNodeWithText("Paired desktop").assertIsDisplayed()
            compose.onNode(hasText("Settings") and bar).assertIsSelected()
            // A destination reached from the bar has no Back arrow in the app bar.
            compose.onNodeWithContentDescription("Back").assertDoesNotExist()
            Espresso.pressBack()
            compose.onNodeWithText("Buildmesh").assertIsDisplayed()
            compose.onNode(hasText("Meshes") and bar).assertIsSelected()

            compose.onNode(hasText("Usage") and bar).performClick()
            compose.onNode(hasText("Usage") and bar).assertIsSelected()
            compose.onNodeWithText("Buildmesh").assertDoesNotExist()
            Espresso.pressBack()
            compose.onNodeWithText("Buildmesh").assertIsDisplayed()
        }
    }

    @Test fun newTaskIsAnActionThatOpensCaptureForTheSelectedMeshAndBackReturns() {
        withApp {
            compose.onNodeWithText("Website").performClick()
            compose.onNodeWithContentDescription("New task").performClick()
            compose.onNodeWithText("Mesh: Website").assertIsDisplayed()
            compose.onNodeWithTag("bottom-navigation").assertDoesNotExist()
            compose.onNodeWithContentDescription("Back").performClick()
            // Capture was opened from the mesh page, so Back returns to it.
            compose.onNodeWithText("Website agent").assertIsDisplayed()
            compose.onNodeWithContentDescription("New task").assertIsDisplayed()
        }
    }

    @Test fun newTaskIsOnlyOfferedOverMeshes() {
        withApp {
            compose.onNodeWithContentDescription("New task").assertIsDisplayed()
            compose.onNode(hasText("Settings") and hasAnyAncestor(hasTestTag("bottom-navigation"))).performClick()
            compose.onNodeWithContentDescription("New task").assertDoesNotExist()
        }
    }

    @Test fun aWideWindowSwapsTheBarForARailAndKeepsBothPanesOnScreen() {
        withApp(density = wideDensity()) {
            compose.onNodeWithTag("navigation-rail").assertIsDisplayed()
            compose.onNodeWithTag("bottom-navigation").assertDoesNotExist()
            listOf("Meshes", "Usage", "Settings").forEach {
                compose.onNode(hasText(it) and hasAnyAncestor(hasTestTag("navigation-rail"))).assertIsDisplayed()
            }
            // Choosing a mesh fills the pane beside the list; it is not a screen
            // to leave, so the list stays and nothing needs Back.
            compose.onNodeWithText("Buildmesh agent").assertDoesNotExist()
            compose.onNodeWithText("Buildmesh").performClick()
            compose.onNodeWithText("Buildmesh agent").assertIsDisplayed()
            compose.onNodeWithText("Website").assertIsDisplayed()
            compose.onNodeWithContentDescription("Back").assertDoesNotExist()
            saveScreenshot("navigation-wide")
            // The rail's head is the one New task button, on every destination.
            compose.onNodeWithContentDescription("New task").performClick()
            compose.onNodeWithText("Mesh: Buildmesh").assertIsDisplayed()
            compose.onNodeWithContentDescription("Back").performClick()
            compose.onNodeWithText("Buildmesh agent").assertIsDisplayed()
            compose.onNode(hasText("Settings") and hasAnyAncestor(hasTestTag("navigation-rail"))).performClick()
            compose.onNodeWithText("Paired desktop").assertIsDisplayed()
            compose.onNodeWithContentDescription("New task").assertIsDisplayed()
        }
    }

    // The rail offers New task on every destination, so Back from Capture has
    // to return to the destination it was started from, not to Meshes.
    @Test fun backFromNewTaskReturnsToTheDestinationItWasStartedFrom() {
        withApp(density = wideDensity()) {
            val rail = hasAnyAncestor(hasTestTag("navigation-rail"))
            compose.onNode(hasText("Settings") and rail).performClick()
            compose.onNodeWithContentDescription("New task").performClick()
            compose.onNodeWithText("Mesh: Buildmesh").assertIsDisplayed()
            compose.onNodeWithContentDescription("Back").performClick()
            compose.onNodeWithText("Paired desktop").assertIsDisplayed()
            compose.onNode(hasText("Settings") and rail).assertIsSelected()

            compose.onNode(hasText("Usage") and rail).performClick()
            compose.onNodeWithContentDescription("New task").performClick()
            compose.onNodeWithText("Mesh: Buildmesh").assertIsDisplayed()
            Espresso.pressBack()
            compose.onNode(hasText("Usage") and rail).assertIsSelected()
            compose.onNodeWithText("Paired desktop").assertDoesNotExist()
        }
    }
}
