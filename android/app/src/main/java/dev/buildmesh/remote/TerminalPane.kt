package dev.buildmesh.remote

import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.Intent
import android.view.KeyEvent
import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Notes
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.KeyboardHide
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.KeyboardCapitalization
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
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.webkit.WebViewAssetLoader
import java.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * The quick keys the terminal's own row offers: the keys a soft keyboard
 * lacks, which the agent's own menus and completion are driven by.
 *
 * Each key's bytes come from [terminalKeyBytes], so the arrows follow the
 * agent's cursor-key mode exactly as a hardware keyboard's arrows do.
 *
 * The approval keys are deliberately absent: a pending request is answered from
 * the band above the terminal, which names the request it answers. A bare `y`
 * in a quick-key row cannot say which request it would approve, so it would
 * approve whichever prompt the agent happens to show next.
 */
val terminalQuickKeys: List<Pair<String, (TerminalInputModes) -> String>> = listOf(
    QuickKey("Esc", KeyEvent.KEYCODE_ESCAPE), QuickKey("Tab", KeyEvent.KEYCODE_TAB),
    QuickKey("Shift+Tab", KeyEvent.KEYCODE_TAB, shift = true),
    QuickKey("Ctrl+C", KeyEvent.KEYCODE_C, char = 'c'.code, ctrl = true), QuickKey("Enter", KeyEvent.KEYCODE_ENTER),
    QuickKey("↑", KeyEvent.KEYCODE_DPAD_UP), QuickKey("↓", KeyEvent.KEYCODE_DPAD_DOWN),
    QuickKey("←", KeyEvent.KEYCODE_DPAD_LEFT), QuickKey("→", KeyEvent.KEYCODE_DPAD_RIGHT),
).map { key ->
    key.label to { modes: TerminalInputModes ->
        checkNotNull(terminalKeyBytes(key.keyCode, key.char, shift = key.shift, ctrl = key.ctrl, modes = modes))
    }
}

private class QuickKey(val label: String, val keyCode: Int, val char: Int = 0,
                       val shift: Boolean = false, val ctrl: Boolean = false)

/**
 * One node's terminal.
 *
 * The pane draws the desktop's semantic-turn banner directly above the
 * terminal surface, where the request is read, so a harness blocked on a
 * person is answered from the screen that shows its prompt (issue #2609).
 * The band is read from the node row's own lifecycle and answers through the
 * desktop's `y\r` / `n\r` keystrokes.
 *
 * Typing goes straight into the agent's own input box (issue #2747): a tap on
 * the terminal opens the keyboard on [TerminalSurface], which sends each key
 * as it is typed. The xterm display itself stays display-only.
 */
@SuppressLint("SetJavaScriptEnabled")
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun TerminalPane(api: BuildmeshApi, nodeId: Long, vm: BuildmeshViewModel) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope { Dispatchers.Main.immediate }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var status by remember { mutableStateOf("Loading terminal…") }
    // The Compose sheet's draft survives closing the sheet and recreation, so
    // dismissing it by accident never loses a long prompt.
    var draft by rememberSaveable(nodeId) { mutableStateOf("") }
    var composing by rememberSaveable(nodeId) { mutableStateOf(false) }
    // Read from the phone's xterm, which tracks the modes the agent switched
    // on in its output (the desktop snapshot replays them on connect).
    var modes by remember(nodeId) { mutableStateOf(TerminalInputModes()) }
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
    // Input reports disconnection instead of silently queueing keystrokes.
    fun sendInput(bytes: String): Boolean = socket.send(bytes).also {
        if (!it) vm.notice("Input was not sent. Reconnect the terminal.")
    }
    // The surface adopts the WebView as its child, so it lives exactly as long
    // as that WebView; a second surface for the same view would find it
    // already parented. Sends go through the latest socket.
    val send by rememberUpdatedState<(String) -> Boolean>({ sendInput(it) })
    val surface = remember(view) { TerminalSurface(context, view, modes = { modes }, send = { send(it) }) }

    // The node's own row is the source of the request: the desktop's
    // semantic-turn banner reads the same lifecycle, so the phone shows the
    // request the desktop shows, in the desktop's own words.
    val state by vm.state.collectAsStateWithLifecycle()
    val request = state.nodes.firstOrNull { it.number("id") == nodeId }?.let { nodeInputRequest(it) }
    // The same reply state the waiting deck uses, so the two surfaces cannot
    // drift apart on the labels, the keystrokes or the moment a control
    // retires.
    val reply = rememberInputReply(request, nodeId, state.busy) { id, sequence, onDone ->
        vm.action(onFailed = { onDone(false) }) { client ->
            client.sendKeys(id, sequence)
            onDone(true)
        }
    }

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
            // A plain tap on the terminal (not a link, not a drag) types into
            // the agent's own input box.
            @JavascriptInterface fun focusInput() { scope.launch {
                if (status == "Connected") surface.showKeyboard()
            } }
            @JavascriptInterface fun modes(applicationCursorKeys: Boolean, bracketedPaste: Boolean) { scope.launch {
                modes = TerminalInputModes(applicationCursorKeys, bracketedPaste)
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
        SemanticTurnBand(request, reply)
        AndroidView(factory = { surface }, modifier = Modifier.fillMaxWidth().weight(1f).onSizeChanged { viewport = it })
        // One row: the keyboard toggle and Compose first, then the keys a soft
        // keyboard lacks. There is no second prompt box — typing goes into
        // the agent's own input, which is where its menus and completion are.
        val connected = status == "Connected"
        val keyboardOpen = WindowInsets.isImeVisible
        Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(enabled = connected, onClick = { if (keyboardOpen) surface.hideKeyboard() else surface.showKeyboard() }) {
                if (keyboardOpen) Icon(Icons.Outlined.KeyboardHide, contentDescription = "Hide keyboard")
                else Icon(Icons.Outlined.Keyboard, contentDescription = "Type into the agent")
            }
            TextButton(enabled = connected, onClick = { surface.hideKeyboard(); composing = true }) {
                Icon(Icons.AutoMirrored.Outlined.Notes, contentDescription = null, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(6.dp))
                Text("Compose")
            }
            VerticalDivider(Modifier.height(24.dp))
            terminalQuickKeys.forEach { (label, bytes) ->
                TextButton(enabled = connected, onClick = { sendInput(bytes(modes)) }) { Text(label) }
            }
        }
    }

    if (composing) TerminalComposeSheet(
        draft = draft, onDraft = { draft = it }, enabled = status == "Connected",
        onDismiss = { composing = false },
        onInsert = { submit ->
            if (sendInput(composePaste(draft, modes))) {
                draft = ""
                composing = false
                // Enter goes separately, after the paste has settled in the
                // agent's box (see COMPOSE_SUBMIT_GAP_MS).
                if (submit) scope.launch { delay(COMPOSE_SUBMIT_GAP_MS); sendInput("\r") }
            }
        },
    )
}

/**
 * Long prompts, swipe typing and dictation.
 *
 * Typing in the terminal asks the keyboard for one character at a time, which
 * turns off autocorrect, swipe and the keyboard's microphone. This sheet gives
 * them back for writing prose, then puts the text into the agent's own input
 * box: **Send** also presses Enter, **Insert** leaves it in the box to edit or
 * complete there.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TerminalComposeSheet(
    draft: String, onDraft: (String) -> Unit, enabled: Boolean,
    onDismiss: () -> Unit, onInsert: (submit: Boolean) -> Unit,
) {
    val focus = remember { FocusRequester() }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 16.dp).imePadding(),
            verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Compose", style = MaterialTheme.typography.titleLarge)
            Text("Goes into the agent's own input box. Your keyboard's autocorrect, swipe and microphone work here.",
                style = MaterialTheme.typography.bodySmall)
            OutlinedTextField(colors = buildmeshTextFieldColors(), value = draft, onValueChange = onDraft,
                label = { Text("Message to the agent") }, minLines = 3, maxLines = 8,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences, autoCorrectEnabled = true),
                modifier = Modifier.fillMaxWidth().focusRequester(focus))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End)) {
                TextButton(enabled = enabled && draft.isNotBlank(), onClick = { onInsert(false) }) { Text("Insert") }
                Button(shape = MaterialTheme.shapes.small, enabled = enabled && draft.isNotBlank(), onClick = { onInsert(true) }) { Text("Send") }
            }
        }
        // The sheet's content lives in its own window and attaches after the
        // first frame; requesting focus before that throws, so wait for it.
        LaunchedEffect(Unit) {
            withFrameNanos { }
            focus.requestFocus()
        }
    }
}
