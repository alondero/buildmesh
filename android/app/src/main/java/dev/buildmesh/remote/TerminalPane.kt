package dev.buildmesh.remote

import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.Intent
import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.net.toUri
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.webkit.WebViewAssetLoader
import java.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

@SuppressLint("SetJavaScriptEnabled")
@Composable
fun TerminalPane(api: BuildmeshApi, nodeId: Long, vm: BuildmeshViewModel) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope { Dispatchers.Main.immediate }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var status by remember { mutableStateOf("Loading terminal…") }
    var input by remember(nodeId) { mutableStateOf("") }
    var cols by remember { mutableIntStateOf(80) }
    var rows by remember { mutableIntStateOf(24) }
    var ready by remember { mutableStateOf(false) }
    var viewport by remember { mutableStateOf(IntSize.Zero) }
    // False once the user has panned the xterm scrollback away from the live
    // tail. xterm's own scroller only answers the mouse wheel, so the phone
    // pans through `attachTouchPan` in the WebView bundle; this carries that
    // position back to the pane so the "latest" control can appear. Keyed by
    // node, because the pane slot is reused when the user switches nodes and
    // a stale tail position would show the control on a fresh terminal.
    var liveTail by remember(nodeId) { mutableStateOf(true) }
    val view = remember(nodeId) { WebView(context) }
    val socket = remember(api, nodeId, view) {
        RemoteSocket(api, scope, "terminal", nodeId,
            onStatus = { status = it },
            onOpen = {
                view.evaluateJavascript("window.buildmeshLayout(); window.buildmeshReset()", null)
            },
            onData = { bytes ->
                view.evaluateJavascript("window.buildmeshOutput('${Base64.getEncoder().encodeToString(bytes)}')", null)
            }, onUnauthorized = vm::unauthorized)
    }
    fun resize() { socket.send(buildJsonObject { put("type", "resize"); put("cols", cols); put("rows", rows) }.toString()) }

    DisposableEffect(view, socket, lifecycle) {
        val loader = WebViewAssetLoader.Builder().addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(context)).build()
        view.setBackgroundColor(MeshColors.background.toArgb())
        view.settings.javaScriptEnabled = true
        view.settings.allowFileAccess = false
        view.settings.allowContentAccess = false
        view.settings.domStorageEnabled = false
        view.webViewClient = object : WebViewClient() {
            override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse =
                loader.shouldInterceptRequest(request.url) ?: WebResourceResponse("text/plain", "utf-8", 403, "Forbidden", emptyMap(), "".byteInputStream())
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) = true
        }
        view.addJavascriptInterface(object {
            @JavascriptInterface fun resize(c: Int, r: Int) { scope.launch {
                cols = c.coerceIn(2, 500); rows = r.coerceIn(2, 500); resize()
            } }
            @JavascriptInterface fun ready() { scope.launch {
                ready = true
                if (lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) socket.start()
            } }
            @JavascriptInterface fun scrollState(atBottom: Boolean) { scope.launch {
                liveTail = atBottom
            } }
            @JavascriptInterface fun openUrl(url: String) { scope.launch {
                val uri = url.toUri().normalizeScheme()
                if (uri.scheme !in listOf("http", "https") || uri.host.isNullOrEmpty()) return@launch
                try {
                    context.startActivity(Intent(Intent.ACTION_VIEW, uri).addCategory(Intent.CATEGORY_BROWSABLE))
                } catch (_: ActivityNotFoundException) {
                    vm.notice("No browser is available to open this link.")
                }
            } }
        }, "BuildmeshTerminal")
        view.loadUrl("https://appassets.androidplatform.net/assets/index.html")
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME && ready) socket.start()
            if (event == Lifecycle.Event.ON_PAUSE) socket.stop()
        }
        lifecycle.addObserver(observer)
        onDispose {
            lifecycle.removeObserver(observer)
            socket.stop()
            view.removeJavascriptInterface("BuildmeshTerminal")
            view.destroy()
        }
    }
    LaunchedEffect(status, cols, rows) { if (status == "Connected") resize() }
    LaunchedEffect(ready, viewport) {
        if (ready && viewport.width > 0 && viewport.height > 0) view.evaluateJavascript("window.buildmeshLayout()", null)
    }

    Column(Modifier.fillMaxSize().imePadding()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(status, style = MaterialTheme.typography.labelMedium)
            Row {
                // Appears only while the user is reading back through the
                // scrollback: the phone has no wheel, so the live tail is
                // otherwise out of reach. It lives in the app bar rather than
                // over the terminal because an `AndroidView` is a real View —
                // it draws above, and takes the touch before, any Compose
                // content that overlaps it.
                if (!liveTail) {
                    TextButton(onClick = { view.evaluateJavascript("window.buildmeshScrollToBottom()", null) }) { Text("Latest") }
                }
                TextButton(onClick = { socket.stop(); socket.start() }) { Text("Reconnect") }
            }
        }
        AndroidView(factory = { view }, modifier = Modifier.fillMaxWidth().weight(1f).onSizeChanged { viewport = it })
        Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 6.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            listOf("Esc" to "\u001b", "Tab" to "\t", "Shift+Tab" to "\u001b[Z", "Ctrl+C" to "\u0003", "Enter" to "\r",
                "↑" to "\u001b[A", "↓" to "\u001b[B", "←" to "\u001b[D", "→" to "\u001b[C", "y" to "y", "n" to "n").forEach { (label, sequence) ->
                TextButton(enabled = status == "Connected", onClick = {
                    if (!socket.send(sequence)) vm.notice("Input was not sent. Reconnect the terminal.")
                }) { Text(label) }
            }
        }
        Row(Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(colors = buildmeshTextFieldColors(), value = input, onValueChange = { input = it }, label = { Text("Terminal input") }, modifier = Modifier.weight(1f), maxLines = 3)
            Button(shape = MaterialTheme.shapes.small, enabled = status == "Connected", onClick = {
                if (socket.send(input + "\r")) input = "" else vm.notice("Input was not sent. Reconnect the terminal.")
            }) { Text("Send") }
        }
    }
}
