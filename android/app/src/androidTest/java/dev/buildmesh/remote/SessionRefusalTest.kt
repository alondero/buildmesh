package dev.buildmesh.remote

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import java.net.InetAddress
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which failures may cost a paired phone its credential (issue #2317).
 *
 * Only a genuine authorization failure does: a 401 on a data route, or a
 * 401/403 from the `/api/session` restore that re-presents the credential. A
 * 403 from a data route is that request being refused, and stays an error
 * message with the session intact — the stored credential is read back from
 * the encrypted store after every case.
 */
class SessionRefusalTest {
    private val prBody = buildJsonObject { put("node_id", 7); put("title", "from the phone") }

    /** Serves the create-PR route one status and keeps every other read healthy. */
    private fun desktop(session: MockResponse, restore: MockResponse = MockResponse().setResponseCode(204)): MockWebServer =
        MockWebServer().also { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = when (request.path?.substringBefore('?')) {
                    "/api/session" -> restore
                    "/api/meshes/2/pr" -> session
                    "/api/nodes" -> MockResponse().setBody(JsonArray(MobileWireFixtures.array("nodes").map { it.jsonObject }.map { JsonObject(it + ("id" to JsonPrimitive(7)) + ("name" to JsonPrimitive("remote agent")) + ("status" to JsonPrimitive("running"))) }).toString())
                    // No socket for these cases: the refresh poll is what proves
                    // the pairing still works.
                    "/api/ws-ticket" -> MockResponse().setResponseCode(503)
                    else -> MockResponse().setBody("[]")
                }
            }
        }

    @Test fun aRefusedDataRouteShowsTheErrorAndKeepsTheStoredSession() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        desktop(MockResponse().setResponseCode(403).setBody("{\"error\":\"That agent node belongs to another mesh\"}")).use { server ->
            server.start(InetAddress.getByName("127.0.0.1"), 0)
            val paired = DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly")
            store.save(paired)
            val vm = withContext(Dispatchers.Main) { BuildmeshViewModel(app) }
            try {
                withTimeout(5_000) { vm.state.first { !it.restoring } }
                assertTrue("restore should have kept the phone paired", vm.state.value.paired)
                assertEquals(paired.cookie, store.read()?.cookie)
                withContext(Dispatchers.Main) { vm.action { it.post("/api/meshes/2/pr", prBody) } }
                withTimeout(5_000) { vm.state.first { it.error.isNotEmpty() } }
                assertEquals("That agent node belongs to another mesh", vm.state.value.error)
                assertTrue("a refused request must not unpair the phone", vm.state.value.paired)
                assertEquals("a refused request must not clear the stored session", paired.cookie, store.read()?.cookie)
                // The pairing is still usable: the next refresh reads the desktop.
                withContext(Dispatchers.Main) { vm.resume() }
                withTimeout(5_000) { vm.state.first { it.nodes.isNotEmpty() } }
                assertEquals(7L, vm.state.value.nodes.single().number("id"))
                assertTrue(vm.state.value.paired)
                assertEquals(paired.cookie, store.read()?.cookie)
            } finally {
                withContext(Dispatchers.Main) { vm.forget() }
                store.clear()
            }
        }
    }

    @Test fun aRejectedSessionOnADataRouteStillClearsTheStoredSession() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        desktop(MockResponse().setResponseCode(401)).use { server ->
            server.start(InetAddress.getByName("127.0.0.1"), 0)
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            val vm = withContext(Dispatchers.Main) { BuildmeshViewModel(app) }
            try {
                withTimeout(5_000) { vm.state.first { !it.restoring } }
                assertTrue(vm.state.value.paired)
                withContext(Dispatchers.Main) { vm.action { it.post("/api/meshes/2/pr", prBody) } }
                withTimeout(5_000) { vm.state.first { !it.paired && it.error.isNotEmpty() } }
                assertNull("a revoked device must lose its stored session", store.read())
                assertEquals("This device is no longer authorized. Scan a fresh invitation from the desktop.", vm.state.value.error)
            } finally {
                withContext(Dispatchers.Main) { vm.forget() }
                store.clear()
            }
        }
    }

    @Test fun aRestoreTheDesktopRefusesStillClearsTheStoredSession() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        // The credential boundary refuses with either status, so both end the pairing.
        listOf(401, 403).forEach { status ->
            store.clear()
            desktop(MockResponse().setBody("[]"), MockResponse().setResponseCode(status)).use { server ->
                server.start(InetAddress.getByName("127.0.0.1"), 0)
                store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
                val vm = withContext(Dispatchers.Main) { BuildmeshViewModel(app) }
                try {
                    withTimeout(5_000) { vm.state.first { !it.restoring && !it.paired && it.error.isNotEmpty() } }
                    assertFalse("restore refused with $status must unpair the phone", vm.state.value.paired)
                    assertNull("restore refused with $status must clear the stored session", store.read())
                    assertEquals("This device is no longer authorized. Scan a fresh invitation from the desktop.", vm.state.value.error)
                } finally {
                    withContext(Dispatchers.Main) { vm.forget() }
                    store.clear()
                }
            }
        }
    }

    @Test fun anUnreachableDesktopKeepsTheStoredSessionForRetry() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.start(InetAddress.getByName("127.0.0.1"), 0)
            val origin = server.url("/").toString()
            val paired = DeviceSession(origin, null, "bm_session=test; Path=/; HttpOnly")
            store.save(paired)
            server.shutdown()
            val vm = withContext(Dispatchers.Main) { BuildmeshViewModel(app) }
            try {
                withTimeout(5_000) { vm.state.first { !it.restoring } }
                assertTrue("a network blip must not cost a valid pairing", vm.state.value.paired)
                assertEquals(paired.cookie, store.read()?.cookie)
            } finally {
                withContext(Dispatchers.Main) { vm.forget() }
                store.clear()
            }
        }
    }
}