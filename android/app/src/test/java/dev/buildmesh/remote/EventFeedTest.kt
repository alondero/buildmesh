package dev.buildmesh.remote

import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import okhttp3.*
// okhttp3 has a `Dispatcher` of its own, so the MockWebServer one is named explicitly.
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test

class EventFeedTest {
    /** A desktop that mints a ticket per request and accepts one events socket. */
    private class Desktop(server: MockWebServer) {
        val sockets = Channel<WebSocket>(Channel.UNLIMITED)
        val closed = Channel<Unit>(Channel.UNLIMITED)
        val mints = AtomicInteger(0)

        init {
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = when (request.path?.substringBefore('?')) {
                    "/api/ws-ticket" -> {
                        mints.incrementAndGet()
                        MockResponse().setBody("""{"ticket":"ticket-${mints.get()}"}""")
                    }
                    "/ws/events" -> MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                        override fun onOpen(webSocket: WebSocket, response: Response) { sockets.trySend(webSocket) }
                        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { closed.trySend(Unit) }
                        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) { closed.trySend(Unit) }
                        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) { closed.trySend(Unit) }
                    })
                    else -> MockResponse().setResponseCode(404)
                }
            }
        }
    }

    /**
     * The feed runs on its own scope, not the test's.
     *
     * A regression that leaves a socket behind would otherwise keep the test's
     * scope alive and hang the suite instead of failing it. The scope is
     * cancelled at the end of each test either way.
     */
    private fun feedFor(server: MockWebServer): Pair<EventFeed, CoroutineScope> {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        return EventFeed(scope) { DeviceSession(server.url("/").toString(), null, "cookie") } to scope
    }

    @Test fun twoOwnersShareOneSocketAndBothSeeTheFrames() = runBlocking {
        MockWebServer().use { server ->
            val desktop = Desktop(server)
            val first = Channel<FeedEvent>(Channel.UNLIMITED)
            val second = Channel<FeedEvent>(Channel.UNLIMITED)
            val (feed, scope) = feedFor(server)
            val a = object : FeedListener { override fun onEvent(event: FeedEvent) { first.trySend(event) } }
            val b = object : FeedListener { override fun onEvent(event: FeedEvent) { second.trySend(event) } }
            try {
                feed.attach(a)
                feed.attach(b)
                val socket = withTimeout(5000) { desktop.sockets.receive() }
                socket.send(
                    """{"type":"agent-lifecycle","session_id":7,"kind":"question_requested","status":"awaiting_input","message":"Which branch?"}"""
                )
                val seen = withTimeout(5000) { first.receive() }
                assertEquals(FeedEvent.Lifecycle(7, "question_requested", "awaiting_input", "Which branch?"), seen)
                assertEquals(seen, withTimeout(5000) { second.receive() })
                assertEquals(
                    "two owners must share one socket, not open one each",
                    1,
                    desktop.mints.get(),
                )
            } finally {
                feed.detach(a)
                feed.detach(b)
                scope.cancel()
                desktop.sockets.cancel()
                desktop.closed.cancel()
                first.cancel()
                second.cancel()
            }
        }
    }

    @Test fun theLastOwnerLeavingClosesTheSocket() = runBlocking {
        MockWebServer().use { server ->
            val desktop = Desktop(server)
            val (feed, scope) = feedFor(server)
            val a = object : FeedListener {}
            val b = object : FeedListener {}
            try {
                feed.attach(a)
                feed.attach(b)
                withTimeout(5000) { desktop.sockets.receive() }
                feed.detach(a)
                // One owner is still listening, so the socket stays open.
                delay(300)
                assertNull("the socket must survive while an owner remains", desktop.closed.tryReceive().getOrNull())
                feed.detach(b)
                withTimeout(5000) { desktop.closed.receive() }
            } finally {
                feed.detach(a)
                feed.detach(b)
                scope.cancel()
                desktop.sockets.cancel()
                desktop.closed.cancel()
            }
        }
    }

    @Test fun aRevokedDeviceStopsTheFeedUntilItIsReloaded() = runBlocking {
        MockWebServer().use { server ->
            val mints = AtomicInteger(0)
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    mints.incrementAndGet()
                    return MockResponse().setResponseCode(401).setBody("""{"error":"revoked"}""")
                }
            }
            val revoked = CompletableDeferred<Unit>()
            val (feed, scope) = feedFor(server)
            val listener = object : FeedListener { override fun onUnauthorized() { revoked.complete(Unit) } }
            try {
                feed.attach(listener)
                withTimeout(5000) { revoked.await() }
                // A revoked device must not keep minting tickets, and a new
                // owner must not restart the feed behind the revocation.
                feed.attach(object : FeedListener {})
                delay(300)
                assertEquals("a revoked device must stop minting tickets", 1, mints.get())
                // Pairing again is the only way back.
                feed.reload()
                delay(300)
                assertEquals("a reload must mint against the new credential", 2, mints.get())
            } finally {
                feed.detach(listener)
                scope.cancel()
            }
        }
    }
}
