package dev.buildmesh.remote

import android.annotation.SuppressLint
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
            TextButton(onClick = { socket.stop(); socket.start() }) { Text("Reconnect") }
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
            OutlinedTextField(value = input, onValueChange = { input = it }, label = { Text("Terminal input") }, modifier = Modifier.weight(1f), maxLines = 3)
            Button(enabled = status == "Connected", onClick = {
                if (socket.send(input + "\r")) input = "" else vm.notice("Input was not sent. Reconnect the terminal.")
            }) { Text("Send") }
        }
    }
}
