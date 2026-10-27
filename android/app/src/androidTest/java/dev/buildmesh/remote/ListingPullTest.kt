package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.json.jsonObject
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.TimeUnit

/**
 * Issues, Archive, Changes, Pull requests and Usage offer the pull gesture,
 * and the pull owns it.
 *
 * A pull re-reads the screen's own route exactly once, so the recorded
 * request count against the mock desktop is the evidence: one more read of
 * that route, and no second read from the same gesture. A failed read keeps
 * its own Retry above the fold, and no screen offers a second, in-list
 * refresh control.
 */
class ListingPullTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private val mesh = MobileWireFixtures.array("meshes").first().jsonObject
    private val providers = MobileWireFixtures.array("providers").map { it.jsonObject }

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
                    if (request.path == "/api/agents/17/git/branch") return MockResponse().setBody(MobileWireFixtures.raw("git_branch"))
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

    /**
     * The pull gesture, dragged down over the screen's own scroller.
     *
     * The finger travels downwards from the top of the list, which is what
     * an Android user does to pull to refresh; the drag is slow, so the
     * gesture is a pull and not a fling, and it crosses the positional
     * threshold the indicator needs.
     */
    private fun pullDown() {
        compose.onAllNodes(hasScrollAction()).onFirst().performTouchInput {
            swipe(
                start = Offset(center.x, top + height * 0.25f),
                end = Offset(center.x, bottom - height * 0.02f),
                durationMillis = 600,
            )
        }
    }

    private fun readsOf(requests: ConcurrentLinkedQueue<RecordedRequest>, path: String) = requests.count { it.path == path }

    /**
     * Wait for the screen's content to land.
     *
     * The condition tolerates a composition that has not registered its root
     * yet: the activity launches asynchronously, so the first poll of a
     * freshly started screen finds no hierarchy and must keep waiting rather
     * than fail the test.
     */
    private fun waitForText(text: String, substring: Boolean = false, timeoutMillis: Long = 10000) {
        compose.waitUntil(timeoutMillis) {
            runCatching { compose.onAllNodesWithText(text, substring = substring).fetchSemanticsNodes().isNotEmpty() }.getOrDefault(false)
        }
    }

    /** No screen offers a second, in-list refresh control. */
    private fun assertNoInListRefreshButton() {
        compose.onAllNodes(hasText("Reload", substring = true)).assertCountEquals(0)
        compose.onAllNodes(hasText("Refresh", substring = true)).assertCountEquals(0)
    }

    private fun saveScreenshot(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val image = instrumentation.uiAutomation.takeScreenshot()
        instrumentation.targetContext.cacheDir.resolve("$name.png").outputStream().use {
            assertTrue(image.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it))
        }
        image.recycle()
    }

    @Test fun theIssuesScreenRereadsItsRouteOnceWhenPulled() {
        val route = "/api/meshes/3/issues"
        withDesktop({ request ->
            if (request.path == route) MockResponse().setBody(MobileWireFixtures.raw("issues_feed")) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(meshes = listOf(mesh), providers = providers), vm, {})
            } }
            waitForText("#42 Fix deadline", substring = true)
            assertEquals(1, readsOf(requests, route))
            assertNoInListRefreshButton()

            pullDown()

            compose.waitUntil(5000) { readsOf(requests, route) == 2 }
            compose.waitForIdle()
            assertEquals("A pull re-reads the issues route exactly once", 2, readsOf(requests, route))
            saveScreenshot("issues-pull-refresh")
        }
    }

    @Test fun theArchiveScreenRereadsItsRouteOnceWhenPulled() {
        val route = "/api/meshes/3/agent-nodes/discover"
        withDesktop({ request ->
            if (request.path == route) MockResponse().setBody(MobileWireFixtures.raw("archived_agent_nodes")) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "archive", state.copy(providers = providers), vm, {})
            } }
            waitForText("Import and resume")
            assertEquals(1, readsOf(requests, route))
            assertNoInListRefreshButton()

            pullDown()

            compose.waitUntil(5000) { readsOf(requests, route) == 2 }
            compose.waitForIdle()
            assertEquals("A pull re-reads the archive route exactly once", 2, readsOf(requests, route))
        }
    }

    @Test fun theChangesScreenRereadsItsRoutesOnceWhenPulled() {
        val status = "/api/agents/17/git/status"
        val branch = "/api/agents/17/git/branch"
        withDesktop({ request ->
            if (request.path == status) MockResponse().setBody("[]") else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { ChangesScreen(vm.api!!, 17, vm, {}) } }
            waitForText("Branch: ", substring = true)
            assertEquals(1, readsOf(requests, status))
            assertEquals(1, readsOf(requests, branch))
            assertNoInListRefreshButton()

            pullDown()

            compose.waitUntil(5000) { readsOf(requests, status) == 2 }
            compose.waitForIdle()
            assertEquals("A pull re-reads the changed-files route exactly once", 2, readsOf(requests, status))
            assertEquals("A pull re-reads the branch route exactly once", 2, readsOf(requests, branch))
            saveScreenshot("changes-pull-refresh")
        }
    }

    @Test fun thePullRequestsScreenRereadsItsRouteOnceWhenPulled() {
        val route = "/api/meshes/3/pulls?state=open"
        withDesktop({ request ->
            if (request.path == route) MockResponse().setBody(MobileWireFixtures.raw("pulls_feed")) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { PullRequestsScreen(vm.api!!, 3, vm, {}) } }
            waitForText("PR #42", substring = true)
            assertEquals(1, readsOf(requests, route))
            assertNoInListRefreshButton()

            pullDown()

            compose.waitUntil(5000) { readsOf(requests, route) == 2 }
            compose.waitForIdle()
            assertEquals("A pull re-reads the pull-requests route exactly once", 2, readsOf(requests, route))
        }
    }

    @Test fun theUsageScreenRereadsItsRouteOnceWhenPulled() {
        withDesktop({ request ->
            if (request.path?.startsWith("/api/usage") == true) MockResponse().setBody(MobileWireFixtures.raw("usage")) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { UsageScreen(vm.api!!, vm) } }
            waitForText("Claude subscription")
            assertEquals(1, requests.count { it.path?.startsWith("/api/usage") == true })
            assertNoInListRefreshButton()

            pullDown()

            compose.waitUntil(5000) { requests.count { it.path?.startsWith("/api/usage") == true } == 2 }
            compose.waitForIdle()
            assertEquals("A pull re-reads the usage route exactly once", 2, requests.count { it.path?.startsWith("/api/usage") == true })
        }
    }

    @Test fun aFailedIssuesReadKeepsItsRetryVisibleWithoutScrolling() {
        withDesktop({ MockResponse().setResponseCode(500).setBody("""{"error":"The desktop is busy"}""") }) { vm, _ ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(meshes = listOf(mesh), providers = providers), vm, {})
            } }
            waitForText("Retry")
            // Displayed, not merely present: the Retry is above the fold.
            compose.onNodeWithText("Retry").assertIsDisplayed()
        }
    }

    @Test fun aFailedArchiveReadKeepsItsRetryVisibleWithoutScrolling() {
        withDesktop({ MockResponse().setResponseCode(500).setBody("""{"error":"The desktop is busy"}""") }) { vm, _ ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "archive", state.copy(providers = providers), vm, {})
            } }
            waitForText("Retry")
            compose.onNodeWithText("Retry").assertIsDisplayed()
        }
    }

    @Test fun aFailedChangesReadKeepsItsRetryVisibleWithoutScrolling() {
        withDesktop({ request ->
            if (request.path == "/api/agents/17/git/branch") MockResponse().setBody(MobileWireFixtures.raw("git_branch"))
            else MockResponse().setResponseCode(500).setBody("""{"error":"The desktop is busy"}""")
        }) { vm, _ ->
            compose.setContent { BuildmeshTheme { ChangesScreen(vm.api!!, 17, vm, {}) } }
            waitForText("Retry")
            compose.onNodeWithText("Retry").assertIsDisplayed()
        }
    }

    @Test fun aFailedPullRequestReadKeepsItsRetryVisibleWithoutScrolling() {
        withDesktop({ MockResponse().setResponseCode(500).setBody("""{"error":"The desktop is busy"}""") }) { vm, _ ->
            compose.setContent { BuildmeshTheme { PullRequestsScreen(vm.api!!, 3, vm, {}) } }
            waitForText("Retry")
            compose.onNodeWithText("Retry").assertIsDisplayed()
        }
    }

    @Test fun aFailedUsageReadKeepsItsRetryVisibleWithoutScrolling() {
        withDesktop({ MockResponse().setResponseCode(500).setBody("""{"error":"The desktop is busy"}""") }) { vm, _ ->
            compose.setContent { BuildmeshTheme { UsageScreen(vm.api!!, vm) } }
            waitForText("Retry")
            compose.onNodeWithText("Retry").assertIsDisplayed()
        }
    }

    /**
     * Phone-width evidence that the pull owns an indicator of its own.
     *
     * The desktop answers slowly, so the read the pull asked for is still in
     * flight when the shutter opens and the pull's indicator is on screen.
     */
    @Test fun thePullShowsItsOwnIndicatorOnTheIssuesScreen() {
        val route = "/api/meshes/3/issues"
        withDesktop({ request ->
            if (request.path == route) MockResponse().setBody(MobileWireFixtures.raw("issues_feed")).setBodyDelay(5, TimeUnit.SECONDS)
            else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(meshes = listOf(mesh), providers = providers), vm, {})
            } }
            waitForText("#42 Fix deadline", substring = true, timeoutMillis = 20000)
            assertEquals(1, readsOf(requests, route))

            pullDown()

            compose.waitUntil(10000) { readsOf(requests, route) == 2 }
            saveScreenshot("issues-pull-indicator")
        }
    }

    @Test fun thePullShowsItsOwnIndicatorOnTheChangesScreen() {
        val status = "/api/agents/17/git/status"
        withDesktop({ request ->
            if (request.path == status) MockResponse().setBody("[]").setBodyDelay(5, TimeUnit.SECONDS) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme { ChangesScreen(vm.api!!, 17, vm, {}) } }
            waitForText("Branch: ", substring = true, timeoutMillis = 20000)
            assertEquals(1, readsOf(requests, status))

            pullDown()

            compose.waitUntil(10000) { readsOf(requests, status) == 2 }
            saveScreenshot("changes-pull-indicator")
        }
    }

    @Test fun theAppBarActionStaysTheRecoveryAffordanceForAListingScreen() {
        val route = "/api/meshes/3/issues"
        withDesktop({ request ->
            if (request.path == route) MockResponse().setBody(MobileWireFixtures.raw("issues_feed")) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(meshes = listOf(mesh), providers = providers), vm, {})
            } }
            waitForText("#42 Fix deadline", substring = true)
            assertEquals(1, readsOf(requests, route))

            compose.runOnUiThread { vm.retry() }

            compose.waitUntil(5000) { readsOf(requests, route) == 2 }
            compose.waitForIdle()
            assertEquals("The app-bar action re-reads the screen's own route", 2, readsOf(requests, route))
        }
    }

    /**
     * The pull and the app-bar action do not share one refresh mechanism.
     *
     * The shared surface is the ViewModel's `resourceRevision` — the revision
     * is what `vm.retry()` moves, and it is also what sets the thin progress
     * bar. A pull must leave it alone, or the pull would drive the app bar's
     * indicator as well as its own.
     */
    @Test fun thePullDoesNotUseTheSharedAppBarRefreshMechanism() {
        val route = "/api/meshes/3/issues"
        withDesktop({ request ->
            if (request.path == route) MockResponse().setBody(MobileWireFixtures.raw("issues_feed")) else MockResponse().setResponseCode(404)
        }) { vm, requests ->
            compose.setContent { BuildmeshTheme {
                val state by vm.state.collectAsState()
                ResourceScreen(vm.api!!, 3, "issues", state.copy(meshes = listOf(mesh), providers = providers), vm, {})
            } }
            waitForText("#42 Fix deadline", substring = true)
            val revision = vm.state.value.resourceRevision

            pullDown()

            compose.waitUntil(5000) { readsOf(requests, route) == 2 }
            compose.waitForIdle()
            compose.runOnIdle {
                assertEquals("The pull re-read the screen's own route", 2, readsOf(requests, route))
                assertEquals("The pull did not move the shared resource revision", revision, vm.state.value.resourceRevision)
                assertFalse("The pull did not claim the app bar's progress bar", vm.state.value.manualRefresh)
            }

            compose.runOnUiThread { vm.retry() }

            compose.waitForIdle()
            compose.runOnIdle {
                assertEquals("The app-bar action moved the shared resource revision", revision + 1, vm.state.value.resourceRevision)
            }
        }
    }
}
