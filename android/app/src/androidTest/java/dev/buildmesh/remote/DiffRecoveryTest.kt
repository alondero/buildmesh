package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Rule
import org.junit.Test
import org.junit.Assert.assertEquals
import java.util.concurrent.TimeUnit

class DiffRecoveryTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test fun failedDiffLeavesLoadingAndRetryLoadsTheActualResponse() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        SessionStore(app).clear()
        // The canonical diff body, with this test's own file, hunk and line.
        val diffFile = MobileWireFixtures.obj("git_diff").arrayAt("files").first()
        val diffHunk = diffFile.arrayAt("hunks").first()
        val diffLine = diffHunk.arrayAt("lines").first()
        val recoveredDiff = JsonObject(
            MobileWireFixtures.obj("git_diff") +
                ("files" to JsonArray(listOf(
                    JsonObject(
                        diffFile +
                            ("path" to JsonPrimitive("file.txt")) +
                            ("hunks" to JsonArray(listOf(
                                JsonObject(
                                    diffHunk +
                                        ("old_start" to JsonPrimitive(1)) +
                                        ("new_start" to JsonPrimitive(1)) +
                                        ("lines" to JsonArray(listOf(
                                            JsonObject(diffLine + ("content" to JsonPrimitive("recovered diff")))
                                        )))
                                )
                            )))
                    )
                )))
        ).toString()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            server.enqueue(MockResponse().setResponseCode(503).setBody("{\"error\":\"temporary desktop failure\"}"))
            server.enqueue(MockResponse().setBody(recoveredDiff))
            val api = BuildmeshApi(server.url("/").toString())
            val vm = BuildmeshViewModel(app)
            try {
                compose.setContent { BuildmeshTheme { DiffScreen(api, 7, "file.txt", vm) } }
                compose.waitUntil(10000) { compose.onAllNodesWithText("Retry").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("Loading…").assertDoesNotExist()
                compose.onNodeWithText("temporary desktop failure").assertExists()
                compose.onNodeWithText("Retry").performScrollTo().performClick()
                compose.waitForIdle()
                compose.waitUntil(10000) { compose.onAllNodesWithText("+recovered diff").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("+recovered diff").assertIsDisplayed()
                assertEquals("/api/agents/7/diff?path=file.txt", server.takeRequest(5, TimeUnit.SECONDS)!!.path)
                assertEquals("/api/agents/7/diff?path=file.txt", server.takeRequest(5, TimeUnit.SECONDS)!!.path)
            } finally { api.close(); compose.runOnIdle { vm.forget() } }
        }
    }
}
