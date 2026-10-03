package dev.buildmesh.remote

import android.app.Application
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okio.ByteString.Companion.encodeUtf8
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class TerminalRenderingTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private fun webView(view: View): WebView? = when (view) {
        is WebView -> view
        is ViewGroup -> (0 until view.childCount).firstNotNullOfOrNull { webView(view.getChildAt(it)) }
        else -> null
    }

    private fun evaluate(view: WebView, script: String): String {
        val result = ArrayBlockingQueue<String>(1)
        compose.runOnUiThread { view.evaluateJavascript(script) { result.offer(it) } }
        return result.poll(5, TimeUnit.SECONDS) ?: error("WebView did not evaluate terminal state")
    }

    @Test fun realTerminalRendersSnapshotAndBinaryOutputWithUsableGeometryAndInput() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        SessionStore(app).clear()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            val input = CountDownLatch(1)
            val frames = ConcurrentLinkedQueue<String>()
            server.enqueue(MockResponse().setBody("{\"ticket\":\"test-socket\"}"))
            server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    webSocket.send("Native snapshot\r\n\u001b[c" + "context line\r\n".repeat(6))
                    webSocket.send("Live Unicode ✓\r\n".encodeUtf8())
                }
                override fun onMessage(webSocket: WebSocket, text: String) {
                    frames.add(text)
                    if (text == "pwd\r") input.countDown()
                }
            }))
            val api = BuildmeshApi(server.url("/").toString())
            val vm = BuildmeshViewModel(app)
            try {
                compose.setContent { BuildmeshTheme { TerminalPane(api, 7, vm) } }
                compose.waitUntil(10000) { compose.onAllNodesWithText("Connected").fetchSemanticsNodes().isNotEmpty() }
                lateinit var view: WebView
                compose.runOnUiThread { view = webView(compose.activity.window.decorView) ?: error("Native terminal missing") }
                try {
                    compose.waitUntil(10000) {
                        evaluate(view, "document.querySelector('.xterm-rows').innerText").contains("Live Unicode")
                    }
                } catch (e: Exception) {
                    throw AssertionError("Terminal did not render live output: " + evaluate(view,
                        "JSON.stringify({rows:document.querySelector('.xterm-rows')?.innerText,screen:document.querySelector('.xterm-screen')?.getBoundingClientRect().toJSON()})"), e)
                }
                val rendered = Json.parseToJsonElement(evaluate(view, "document.querySelector('.xterm-rows').innerText")).jsonPrimitive.content
                assertTrue(rendered.contains("Native snapshot"))
                assertTrue(rendered.contains("Live Unicode ✓"))
                assertTrue(evaluate(view, "document.querySelector('.xterm-screen').getBoundingClientRect().height").toDouble() > 100)
                val position = IntArray(2)
                compose.runOnUiThread { view.getLocationOnScreen(position) }
                compose.waitUntil(10000) {
                    val screen = InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot()
                        ?: error("Could not capture the native terminal")
                    try {
                        val width = view.width.coerceAtMost(screen.width - position[0])
                        val height = view.height.coerceAtMost(screen.height - position[1])
                        val pixels = IntArray(width * height)
                        screen.getPixels(pixels, 0, width, position[0], position[1], width, height)
                        pixels.count {
                            android.graphics.Color.red(it) > 150 && android.graphics.Color.green(it) > 150 && android.graphics.Color.blue(it) > 150
                        } > 200
                    } finally { screen.recycle() }
                }
                assertTrue(frames.filter { it.startsWith('{') }.map { Json.parseToJsonElement(it).obj() }
                    .any { it.text("type") == "resize" && it.number("cols") > 10 && it.number("rows") > 5 })
                compose.onNode(hasSetTextAction()).performTextInput("pwd")
                compose.onNodeWithText("Send").performClick()
                assertTrue(input.await(5, TimeUnit.SECONDS))
                listOf("Shift+Tab", "←", "→", "y", "n", "Enter").forEach {
                    compose.onNodeWithText(it).performScrollTo().performClick()
                }
                compose.waitUntil(5000) { frames.count { !it.startsWith('{') } >= 7 }
                assertEquals("Only explicit native input reaches the PTY", listOf("pwd\r", "\u001b[Z", "\u001b[D", "\u001b[C", "y", "n", "\r"),
                    frames.filter { !it.startsWith('{') })
                assertEquals("/api/ws-ticket", server.takeRequest(5, TimeUnit.SECONDS)!!.path)
                assertEquals("/ws/terminal/7?ticket=test-socket", server.takeRequest(5, TimeUnit.SECONDS)!!.path)
            } finally {
                compose.runOnUiThread { vm.forget() }
                api.close()
            }
        }
    }
}
