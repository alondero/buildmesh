package dev.buildmesh.remote

import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import okhttp3.*
// okhttp3 has a `Dispatcher` of its own, so the MockWebServer one is named explicitly.
import okhttp3.mockwebserver.Dispatcher
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

    @Test fun aRefusedHandshakeMintsAFreshTicketInsteadOfRevokingTheDevice() = runBlocking {
        MockWebServer().use { server ->
            // Every handshake is refused: 403 is a ticket minted for another
            // target, and 401 a ticket that expired or was already used. Neither
            // is the desktop's verdict on this device (issue #2317).
            listOf(403, 401).forEach { status ->
                val mints = Channel<Int>(Channel.UNLIMITED)
                server.dispatcher = object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse = when (request.path?.substringBefore('?')) {
                        "/api/ws-ticket" -> { mints.trySend(1); MockResponse().setBody("{\"ticket\":\"ticket\"}") }
                        "/ws/events" -> MockResponse().setResponseCode(status)
                        else -> MockResponse().setResponseCode(404)
                    }
                }
                val revoked = CompletableDeferred<Unit>()
                val api = BuildmeshApi(server.url("/").toString())
                val socket = RemoteSocket(api, this, "events", onData = { fail("Unexpected output") }, onUnauthorized = { revoked.complete(Unit) })
                socket.start()
                try {
                    // Three mints prove a refused upgrade is retried with a fresh
                    // ticket instead of ending the session.
                    withTimeout(20000) { repeat(3) { mints.receive() } }
                    assertFalse("a refused handshake must not revoke the device ($status)", revoked.isCompleted)
                } finally { socket.stop(); api.close(); mints.cancel() }
            }
        }
    }
}
