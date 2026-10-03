package dev.buildmesh.remote

import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import okhttp3.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Test

class RemoteSocketTest {
    @Test fun stopThenImmediateRestartKeepsNewSocketAndRejectsOldOutput() = runBlocking {
        MockWebServer().use { server ->
            val servers = Channel<WebSocket>(Channel.UNLIMITED)
            val received = Channel<String>(Channel.UNLIMITED)
            val statuses = Channel<String>(Channel.UNLIMITED)
            repeat(2) { index ->
                server.enqueue(MockResponse().setBody("{\"ticket\":\"ticket-$index\"}"))
                server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                    override fun onOpen(webSocket: WebSocket, response: Response) { servers.trySend(webSocket) }
                }))
            }
            val api = BuildmeshApi(server.url("/").toString())
            val socket = RemoteSocket(api, this, "terminal", 7, onStatus = { statuses.trySend(it) },
                onData = { received.trySend(it.toString(Charsets.UTF_8)) }, onUnauthorized = { fail("Unexpected unauthorized") })
            try {
                socket.start()
                val first = withTimeout(5000) { servers.receive() }
                withTimeout(5000) { while (statuses.receive() != "Connected") { } }
                first.send("first")
                assertEquals("first", withTimeout(5000) { received.receive() })
                socket.stop(); socket.start()
                val second = withTimeout(5000) { servers.receive() }
                withTimeout(5000) { while (statuses.receive() != "Connected") { } }
                first.send("stale")
                second.send("new")
                assertEquals("new", withTimeout(5000) { received.receive() })
                assertTrue(socket.send("input"))
                val paths = List(4) { server.takeRequest(5, TimeUnit.SECONDS)!!.path }
                assertEquals(listOf("/api/ws-ticket", "/ws/terminal/7?ticket=ticket-0", "/api/ws-ticket", "/ws/terminal/7?ticket=ticket-1"), paths)
            } finally { socket.stop(); api.close(); servers.cancel(); received.cancel(); statuses.cancel() }
        }
    }

    @Test fun authenticationFailureStopsReconnectAndBackgroundStopSendsNothing() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(401).setBody("{\"error\":\"revoked\"}"))
            val revoked = CompletableDeferred<Unit>()
            val api = BuildmeshApi(server.url("/").toString())
            val socket = RemoteSocket(api, this, "events", onData = { fail("Unexpected output") }, onUnauthorized = { revoked.complete(Unit) })
            socket.start()
            withTimeout(5000) { revoked.await() }
            socket.stop()
            assertFalse(socket.send("unsent"))
            assertEquals(1, server.requestCount)
            api.close()
        }
    }
}
