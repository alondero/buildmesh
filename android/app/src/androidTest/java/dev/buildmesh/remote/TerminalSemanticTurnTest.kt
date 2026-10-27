package dev.buildmesh.remote

import android.app.Application
import android.graphics.Rect
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import kotlinx.serialization.json.Json
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import java.net.InetAddress
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit

/**
 * The terminal screen shows the semantic-turn banner above the terminal
 * surface (issue #2609).
 *
 * The desktop draws the pending request directly above the terminal, with the
 * approval pair it asked for. The phone draws the same band in the same place,
 * answers with the same `y\r` / `n\r` keystrokes, and retires the pair only
 * once the desktop accepted the reply.
 */
class TerminalSemanticTurnTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    /** The node row the desktop serves: a harness blocked on a permission. */
    private val waitingNode = Json.parseToJsonElement("""
        {"id":9,"mesh_id":1,"position":0,"name":"Waiting agent","branch":"main","status":"awaiting_input",
         "lifecycle":{"session_id":9,"provider":"claude","kind":"permission_requested","status":"awaiting_input",
           "message":"Claude needs permission to use Bash","provider_event":null,"provider_session_id":null,
           "completion_reason":null,"transcript_path":null,"timestamp":"2026-10-27T10:00:00Z","signal_health":"ok",
           "semantic_turn":{"node_id":9,"kind":"permission_request","description":"Allow Bash: ls -la"}}}
    """.trimIndent()).obj()

    /**
     * The desktop, as the phone sees it.
     *
     * It answers the pairing handshake, mints a socket ticket and — the part
     * this test is about — records every reply the phone sends, so the test
     * can assert on the bytes the phone actually posted.
     */
    private class Desktop : Dispatcher() {
        val replies = CopyOnWriteArrayList<RecordedRequest>()

        /** Whether the desktop refuses the next reply. */
        @Volatile var refuse = true

        override fun dispatch(request: RecordedRequest): MockResponse = when {
            request.path == "/api/pair" -> MockResponse().setResponseCode(204)
                .addHeader("Set-Cookie", "bm_session=bm_session=terminal-band; Path=/; HttpOnly")
            request.path == "/api/ws-ticket" -> MockResponse().setBody("{\"ticket\":\"test-socket\"}")
            request.path?.startsWith("/ws/terminal/") == true ->
                MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                    override fun onOpen(webSocket: WebSocket, response: Response) = Unit
                })
            request.path?.endsWith("/input") == true -> {
                replies.add(request)
                if (refuse) MockResponse().setResponseCode(503).setBody("{\"error\":\"The desktop refused this reply.\"}")
                else MockResponse().setResponseCode(204)
            }
            else -> MockResponse().setResponseCode(404)
        }
    }

    private fun webView(view: View): WebView? = when (view) {
        is WebView -> view
        is ViewGroup -> (0 until view.childCount).firstNotNullOfOrNull { webView(view.getChildAt(it)) }
        else -> null
    }

    /** The native terminal's bounds, in the compose root's coordinates. */
    private fun terminalBoundsInRoot(): Rect {
        lateinit var view: WebView
        lateinit var bounds: Rect
        compose.runOnUiThread {
            view = webView(compose.activity.window.decorView) ?: error("Native terminal missing")
            val content = compose.activity.findViewById<View>(android.R.id.content)
            val terminal = IntArray(2)
            val root = IntArray(2)
            view.getLocationInWindow(terminal)
            content.getLocationInWindow(root)
            bounds = Rect(terminal[0] - root[0], terminal[1] - root[1], terminal[0] - root[0] + view.width, terminal[1] - root[1] + view.height)
        }
        return bounds
    }

    @Test fun theTerminalShowsThePendingRequestAndRetiresItsPairOnlyAfterTheDesktopAccepts() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        SessionStore(app).clear()
        val desktop = Desktop()
        MockWebServer().use { server ->
            server.dispatcher = desktop
            server.start(InetAddress.getByName("127.0.0.1"), 0)
            val vm = BuildmeshViewModel(app)
            try {
                val invitation = "${server.url("/")}#pair=${"a".repeat(64)}"
                compose.runOnUiThread { vm.pair(invitation, "") }
                compose.waitUntil(10000) { vm.state.value.paired && !vm.state.value.busy }
                // The node row the desktop serves is what the band reads.
                vm.acceptNode(waitingNode)
                compose.setContent { BuildmeshTheme { TerminalPane(vm.api!!, 9, vm) } }

                // The request's own words, and the desktop's pair.
                compose.waitUntil(10000) { compose.onAllNodesWithText("Allow Bash: ls -la").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("Allow (Y)").assertExists()
                compose.onNodeWithText("Reject (N)").assertExists()

                // The band sits above the terminal surface, not below it.
                val band = compose.onNodeWithText("Allow (Y)").fetchSemanticsNode().boundsInRoot
                val terminal = terminalBoundsInRoot()
                assertTrue("The band must sit above the terminal surface, was $band against $terminal", band.bottom <= terminal.top)

                // The desktop refuses the first reply.
                compose.onNodeWithText("Allow (Y)").performClick()
                compose.waitUntil(10000) { desktop.replies.size == 1 }
                val refused = desktop.replies[0]
                assertEquals("POST", refused.method)
                assertEquals("/api/nodes/9/input", refused.path)
                assertEquals("{\"seq\":\"y\\r\"}", refused.body.readUtf8())

                // A refused reply hands the control back instead of retiring it.
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Allow (Y)").fetchSemanticsNodes().isNotEmpty() &&
                        runCatching { compose.onNodeWithText("Allow (Y)").assertIsEnabled() }.isSuccess
                }
                assertTrue("A refused reply must not retire the pair",
                    compose.onAllNodesWithText("Approved ✓").fetchSemanticsNodes().isEmpty())

                // The desktop accepts the next one.
                desktop.refuse = false
                compose.onNodeWithText("Allow (Y)").performClick()
                compose.waitUntil(10000) { desktop.replies.size == 2 }
                assertEquals("{\"seq\":\"y\\r\"}", desktop.replies[1].body.readUtf8())
                compose.waitUntil(10000) { compose.onAllNodesWithText("Approved ✓").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("Approved ✓").assertIsNotEnabled()
            } finally {
                compose.runOnUiThread { vm.forget() }
                vm.api?.close()
            }
        }
    }
}
