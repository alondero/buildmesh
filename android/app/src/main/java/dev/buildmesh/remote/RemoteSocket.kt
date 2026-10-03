package dev.buildmesh.remote

import java.io.IOException
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import okhttp3.*
import okio.ByteString

/** One foreground owner, one socket and one reconnect timer; every reconnect mints a fresh ticket. */
class RemoteSocket(
    private val api: BuildmeshApi,
    private val scope: CoroutineScope,
    private val surface: String,
    private val nodeId: Long? = null,
    private val onStatus: (String) -> Unit = {},
    private val onOpen: () -> Unit = {},
    private val onData: (ByteArray) -> Unit,
    private val onUnauthorized: () -> Unit,
) {
    private var job: Job? = null
    private var socket: WebSocket? = null
    private var connected = false
    private var generation = 0

    fun start() {
        if (job?.isActive == true) return
        val owner = ++generation
        job = scope.launch {
            var failures = 0
            while (isActive) {
                onStatus(if (failures == 0) "Connecting…" else "Reconnecting…")
                val messages = Channel<SocketMessage>(64)
                fun enqueue(message: SocketMessage) {
                    if (messages.trySend(message).isFailure) messages.close(IOException("Output backlog requires a fresh snapshot"))
                }
                var attemptSocket: WebSocket? = null
                try {
                    val ticket = api.ticket(surface, nodeId)
                    attemptSocket = api.http.newWebSocket(api.socketRequest(surface, ticket, nodeId), object : WebSocketListener() {
                        override fun onOpen(webSocket: WebSocket, response: Response) { enqueue(SocketMessage.Open) }
                        override fun onMessage(webSocket: WebSocket, text: String) { enqueue(SocketMessage.Data(text.toByteArray(Charsets.UTF_8))) }
                        override fun onMessage(webSocket: WebSocket, bytes: ByteString) { enqueue(SocketMessage.Data(bytes.toByteArray())) }
                        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) { messages.close(IOException("Connection closed")) }
                        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { webSocket.close(code, reason) }
                        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                            messages.close(if (response?.code in setOf(401, 403)) ApiException(response!!.code, "Pair this device again.") else t)
                        }
                    })
                    socket = attemptSocket
                    for (message in messages) {
                        ensureActive()
                        if (owner != generation) break
                        when (message) {
                            SocketMessage.Open -> {
                                connected = true
                                failures = 0
                                onStatus("Connected")
                                onOpen()
                            }
                            is SocketMessage.Data -> onData(message.bytes)
                        }
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    if (e is ApiException && e.unauthorized) {
                        onUnauthorized()
                        break
                    }
                    onStatus("Disconnected · retrying")
                } finally {
                    attemptSocket?.cancel()
                    if (owner == generation) {
                        connected = false
                        socket = null
                    }
                    messages.cancel()
                }
                failures = (failures + 1).coerceAtMost(5)
                delay((1L shl (failures - 1)) * 1000)
            }
        }
    }

    fun send(data: String): Boolean = connected && socket?.send(data) == true

    fun stop() {
        generation++
        job?.cancel()
        job = null
        connected = false
        socket?.cancel()
        socket = null
    }

    private sealed interface SocketMessage {
        data object Open : SocketMessage
        data class Data(val bytes: ByteArray) : SocketMessage
    }
}
