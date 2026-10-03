package dev.buildmesh.remote

import android.app.Application
import android.os.Bundle
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import java.io.ByteArrayOutputStream
import org.junit.Assert.*
import org.junit.Test

/** Explicit live variant: requires a private fixture and never silently skips a missing desktop. */
class LiveDesktopTest {
    @Test fun directLanPairingRefreshTerminalRestoreAndRevocation() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val fixtureFile = app.filesDir.resolve("live-desktop.json")
        check(fixtureFile.exists()) { "Install a live desktop fixture in the test app's private files first." }
        val fixture = Json.parseToJsonElement(fixtureFile.readText()).obj()
        fixtureFile.delete()
        val invitation = Invitation.parse(fixture.text("invitation"))
        check(invitation.origin.startsWith("https://")) { "Live acceptance requires HTTPS." }
        val nodeId = fixture.number("nodeId")
        val meshId = fixture.number("meshId")
        val marker = fixture.text("marker")
        check(marker.matches(Regex("[A-Z0-9_]+")))
        val before = (fixture["beforeDeviceIds"] as JsonArray).map { it.jsonPrimitive.long }.toSet()
        val root = withContext(Dispatchers.IO) { downloadRoot(invitation) }
        val store = SessionStore(app)
        store.clear()
        val api = BuildmeshApi(invitation.origin, root, persist = store::save)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        var vm: BuildmeshViewModel? = null
        var terminal: RemoteSocket? = null
        try {
            api.pair(invitation.ticket)
            assertEquals(invitation.origin, store.read()!!.origin)
            val devices = api.get("/admin/devices").objects().filter { it.number("id") !in before }
            assertEquals("Only this fixture's device may be revoked", 1, devices.size)
            val deviceId = devices.single().number("id")
            InstrumentationRegistry.getInstrumentation().sendStatus(2, Bundle().apply { putLong("buildmeshDeviceId", deviceId) })
            repeat(20) {
                coroutineScope {
                    val meshes = async { api.get("/api/meshes").objects() }
                    val nodes = async { api.get("/api/nodes").objects() }
                    val providers = async { api.get("/api/providers").objects() }
                    assertTrue(meshes.await().any { it.number("id") == meshId })
                    assertTrue(nodes.await().any { it.number("id") == nodeId })
                    assertTrue(providers.await().isNotEmpty())
                }
            }
            val saved = store.read()!!
            BuildmeshApi(saved.origin, saved.root, saved.cookie).let { restored ->
                try { restored.restore() } finally { restored.close() }
            }
            val created = api.post("/api/nodes/create", buildJsonObject {
                put("mesh_id", meshId); put("provider", "terminal"); put("rows", 24); put("cols", 80)
            }).obj()
            assertTrue(created.number("id") > 0)
            assertEquals(meshId, created.number("mesh_id"))
            assertEquals("terminal", created.text("provider"))
            assertTrue(api.get("/api/nodes").objects().any { it.number("id") == created.number("id") })
            val model = withContext(Dispatchers.Main) { BuildmeshViewModel(app).also { it.resume() } }
            vm = model
            withTimeout(15000) { model.state.first { !it.restoring && it.nodes.any { node -> node.number("id") == nodeId } } }
            withContext(Dispatchers.Main) { model.pause() }
            delay(250)
            withContext(Dispatchers.Main) { model.resume() }
            withTimeout(15000) { model.state.first { !it.refreshing && it.nodes.any { node -> node.number("id") == nodeId } && it.error.isEmpty() } }

            val commandOutput = CompletableDeferred<Unit>()
            val reopened = CompletableDeferred<Unit>()
            val output = ByteArrayOutputStream()
            var opens = 0
            lateinit var socket: RemoteSocket
            socket = RemoteSocket(api, scope, "terminal", nodeId,
                onOpen = {
                    opens++
                    if (opens == 1) {
                        assertTrue(socket.send(buildJsonObject { put("type", "resize"); put("cols", 120); put("rows", 24) }.toString()))
                        assertTrue(socket.send("echo $marker\r"))
                    } else reopened.complete(Unit)
                }, onData = { bytes ->
                    output.write(bytes)
                    if (Regex(Regex.escape(marker)).findAll(output.toString("UTF-8")).count() >= 2) commandOutput.complete(Unit)
                }, onUnauthorized = { commandOutput.completeExceptionally(AssertionError("Terminal was unexpectedly unauthorized")) })
            terminal = socket
            withContext(Dispatchers.Main) { socket.start() }
            withTimeout(20000) { commandOutput.await() }
            withContext(Dispatchers.Main) { socket.stop(); socket.start() }
            withTimeout(15000) { reopened.await() }

            val changes = api.get("/api/agents/$nodeId/git/status").objects()
            assertTrue(changes.any { it.text("path") == "smoke.txt" })
            val diff = api.get("/api/agents/$nodeId/diff?path=smoke.txt").obj()
            assertTrue(diff.arrayAt("files").flatMap { it.arrayAt("hunks") }.flatMap { it.arrayAt("lines") }
                .any { it.text("line_type") == "add" && it.text("content").contains("Android direct LAN") })

            api.post("/admin/devices/$deviceId/revoke", buildJsonObject {})
            withTimeout(15000) { model.state.first { !it.paired && it.error.contains("no longer authorized") } }
            assertNull(store.read())
            val failure = runCatching { api.restore() }.exceptionOrNull()
            assertTrue(failure is ApiException && failure.unauthorized)
        } finally {
            withContext(Dispatchers.Main) { terminal?.stop(); vm?.forget() }
            scope.cancel()
            api.close()
            store.clear()
        }
    }
}
