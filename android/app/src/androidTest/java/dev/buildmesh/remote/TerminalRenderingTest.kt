package dev.buildmesh.remote

import android.app.Application
import android.app.Activity
import android.app.Instrumentation
import android.content.Intent
import android.content.IntentFilter
import android.os.SystemClock
import android.view.MotionEvent
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
import kotlinx.serialization.json.jsonArray
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
import java.util.concurrent.atomic.AtomicReference

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

    @Test fun terminalLinkTapsDispatchBrowserIntentsWithoutNavigatingTheWebView() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        SessionStore(app).clear()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            server.enqueue(MockResponse().setBody("{\"ticket\":\"link-test\"}"))
            server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    webSocket.send("https://example.com/plain\r\n\u001b]8;;https://example.com/hyper\u0007Open browser\u001b]8;;\u0007\r\n")
                }
            }))
            val api = BuildmeshApi(server.url("/").toString())
            val vm = BuildmeshViewModel(app)
            try {
                compose.setContent { BuildmeshTheme { TerminalPane(api, 7, vm) } }
                compose.waitUntil(10000) { compose.onAllNodesWithText("Connected").fetchSemanticsNodes().isNotEmpty() }
                lateinit var view: WebView
                compose.runOnUiThread { view = webView(compose.activity.window.decorView) ?: error("Native terminal missing") }
                // Block the browser activity while checking the real WebView
                // touch -> JS bridge -> ACTION_VIEW handoff on this device.
                val instrumentation = InstrumentationRegistry.getInstrumentation()
                val browserFilter = IntentFilter(Intent.ACTION_VIEW).apply {
                    addCategory(Intent.CATEGORY_BROWSABLE)
                    addDataScheme("https")
                    addDataScheme("javascript")
                }
                val browser = instrumentation.addMonitor(browserFilter,
                    Instrumentation.ActivityResult(Activity.RESULT_OK, null), true)
                try {
                    compose.waitUntil(10000) {
                        evaluate(view, "document.querySelector('.xterm-rows').innerText")
                            .replace('\u00a0', ' ').contains("Open browser")
                    }
                    listOf("https://example.com/plain", "Open browser").forEachIndexed { index, label ->
                        val coordinates = Json.parseToJsonElement(Json.parseToJsonElement(evaluate(view, """
                            JSON.stringify((() => {
                              const row = [...document.querySelectorAll('.xterm-rows > div')].find(el => el.textContent.replace(/\s/g, ' ').includes('$label'));
                              const box = row.getBoundingClientRect();
                              return [box.x + 5, box.y + box.height / 2];
                            })())
                        """)).jsonPrimitive.content).jsonArray.map { it.jsonPrimitive.content.toFloat() }
                        compose.runOnUiThread {
                            val now = SystemClock.uptimeMillis()
                            listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP).forEachIndexed { offset, action ->
                                val event = MotionEvent.obtain(now, now + offset * 50, action,
                                    coordinates[0] * view.resources.displayMetrics.density,
                                    coordinates[1] * view.resources.displayMetrics.density, 0)
                                view.dispatchTouchEvent(event)
                                event.recycle()
                            }
                        }
                        compose.waitUntil(5000) { browser.hits == index + 1 }
                    }
                    assertEquals("https://appassets.androidplatform.net/assets/index.html",
                        Json.parseToJsonElement(evaluate(view, "location.href")).jsonPrimitive.content)
                    evaluate(view, "window.BuildmeshTerminal.openUrl('HTTPS://example.com/upper')")
                    compose.waitUntil(5000) { browser.hits == 3 }
                    evaluate(view, "window.BuildmeshTerminal.openUrl('javascript:alert(1)')")
                    compose.waitForIdle()
                    assertEquals("Non-HTTP links cannot launch an activity", 3, browser.hits)
                } finally {
                    instrumentation.removeMonitor(browser)
                }

            } finally {
                compose.runOnUiThread { vm.forget() }
                api.close()
            }
        }
    }

    @Test fun realTerminalRendersSnapshotAndBinaryOutputWithUsableGeometryAndInput() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        SessionStore(app).clear()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            val input = CountDownLatch(1)
            val frames = ConcurrentLinkedQueue<String>()
            val live = AtomicReference<WebSocket?>()
            server.enqueue(MockResponse().setBody("{\"ticket\":\"test-socket\"}"))
            server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    live.set(webSocket)
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
                listOf("Shift+Tab", "←", "→", "Enter").forEach {
                    compose.onNodeWithText(it).performScrollTo().performClick()
                }
                compose.waitUntil(5000) { frames.count { !it.startsWith('{') } >= 5 }
                assertEquals("Only explicit native input reaches the PTY", listOf("pwd\r", "\u001b[Z", "\u001b[D", "\u001b[C", "\r"),
                    frames.filter { !it.startsWith('{') })
                // The approval keys live in the band above the terminal now,
                // where the request they answer is named. A bare `y` in the
                // quick-key row would approve whichever prompt the agent
                // happens to show next.
                listOf("y", "n").forEach {
                    assertTrue("The $it quick key must not be offered",
                        compose.onAllNodesWithText(it).fetchSemanticsNodes().isEmpty())
                }

                // Touch scrolling (the phone has no wheel, so the xterm scrollback
                // is only reachable through the shared touch-pan bridge). Fill the
                // scrollback with numbered lines first, so the rendered rows say
                // which part of the history is on screen.
                val socket = live.get() ?: error("Terminal socket never opened")
                socket.send((0 until 600).joinToString("") { "history $it\r\n" })
                compose.waitUntil(10000) {
                    evaluate(view, "document.querySelector('.xterm-rows').innerText").contains("history 599")
                }
                // The finger has to travel DOWN to walk back through the history:
                // the shared bridge maps a downward drag to a negative scroll,
                // which is the direction that reaches older output. Dragging up
                // moves toward the newest line, which is a no-op at the tail.
                val dragged = evaluate(view, """
                    (function () {
                      const el = document.querySelector('.xterm');
                      if (!el) return 'no .xterm';
                      const touch = (type, y) => {
                        const t = new Touch({identifier: 1, target: el, clientX: 10, clientY: y});
                        return new TouchEvent(type, {touches: [t], targetTouches: [t], changedTouches: [t], bubbles: true, cancelable: true});
                      };
                      el.dispatchEvent(touch('touchstart', 120));
                      el.dispatchEvent(touch('touchmove', 500));
                      el.dispatchEvent(touch('touchend', 500));
                      return 'dragged';
                    })()
                """)
                assertTrue("Touch drag did not reach the terminal surface: $dragged", dragged.contains("dragged"))
                // The last line has left the screen, so the viewport really is
                // back in the history rather than still pinned to the tail.
                compose.waitUntil(10000) {
                    !evaluate(view, "document.querySelector('.xterm-rows').innerText").contains("history 599")
                }
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Latest").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Latest").performClick()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Latest").fetchSemanticsNodes().isEmpty()
                }
                compose.waitUntil(10000) {
                    evaluate(view, "document.querySelector('.xterm-rows').innerText").contains("history 599")
                }

                assertEquals("/api/ws-ticket", server.takeRequest(5, TimeUnit.SECONDS)!!.path)
                assertEquals("/ws/terminal/7?ticket=test-socket", server.takeRequest(5, TimeUnit.SECONDS)!!.path)
            } finally {
                compose.runOnUiThread { vm.forget() }
                api.close()
            }
        }
    }
}
