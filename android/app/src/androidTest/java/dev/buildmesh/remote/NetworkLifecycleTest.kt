package dev.buildmesh.remote

import android.app.Application
import android.os.StrictMode
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class NetworkLifecycleTest {
    @Test fun closingTlsFromMainDoesNotPerformNetworkIo() = runBlocking {
        val root = HeldCertificate.Builder().certificateAuthority(1).build()
        val leaf = HeldCertificate.Builder().commonName("localhost").addSubjectAlternativeName("localhost").signedBy(root).build()
        val identity = HandshakeCertificates.Builder().heldCertificate(leaf, root.certificate).build()
        MockWebServer().use { server ->
            server.useHttps(identity.sslSocketFactory(), false)
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            server.enqueue(MockResponse().setBody("{\"secure\":true}"))
            // Keep a pooled TLS socket to exercise cleanup even when the desktop closes HTTP connections.
            val pooled = httpClient(root.certificate).newBuilder().addInterceptor { chain ->
                chain.proceed(chain.request().newBuilder().removeHeader("Connection").build())
            }.build()
            val api = BuildmeshApi("https://localhost:${server.port}", root.certificate.encoded, client = pooled)
            try {
                withContext(Dispatchers.Main) {
                    assertTrue(api.get("/secure").obj().flag("secure"))
                    val previous = StrictMode.getThreadPolicy()
                    StrictMode.setThreadPolicy(StrictMode.ThreadPolicy.Builder().detectNetwork().penaltyDeathOnNetwork().build())
                    try { api.close() } finally { StrictMode.setThreadPolicy(previous) }
                }
            } finally { withContext(Dispatchers.IO) { api.close() } }
        }
    }

    @Test fun forgettingCancelsACompletedMutationBeforeItsMainCallback() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            server.enqueue(MockResponse().setResponseCode(204))
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            val vm = withContext(Dispatchers.Main) { BuildmeshViewModel(app) }
            withTimeout(5000) { vm.state.first { !it.restoring } }
            val entered = CountDownLatch(1)
            val release = CountDownLatch(1)
            val returned = CountDownLatch(1)
            val finished = CompletableDeferred<Unit>()
            var navigated = false
            try {
                withContext(Dispatchers.Main) {
                    vm.saveDraft("task", "keep my task")
                    vm.action {
                        currentCoroutineContext().job.invokeOnCompletion { finished.complete(Unit) }
                        // Model a completed IO response whose Main continuation has not run yet.
                        withContext(Dispatchers.IO) {
                            entered.countDown()
                            check(release.await(5, TimeUnit.SECONDS))
                            returned.countDown()
                        }
                        vm.saveDraft("task", "")
                        vm.acceptNode(buildJsonObject { put("id", 7) })
                        navigated = true
                    }
                }
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                withContext(Dispatchers.Main) {
                    release.countDown()
                    check(returned.await(5, TimeUnit.SECONDS))
                    vm.forget()
                }
                withTimeout(5000) { finished.await() }
                assertEquals("keep my task", vm.draft("task"))
                assertTrue(vm.state.value.nodes.isEmpty())
                assertFalse(vm.state.value.paired)
                assertFalse(navigated)
            } finally {
                release.countDown()
                withContext(Dispatchers.Main) { vm.forget(); vm.saveDraft("task", "") }
                store.clear()
            }
        }
    }

    @Test fun largeDelayedBodiesCanBeReadFromMainWithNetworkStrictMode() = runBlocking {
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            server.enqueue(MockResponse().setBody("{\"diff\":\"${"x".repeat(64_000)}\"}").setBodyDelay(100, TimeUnit.MILLISECONDS))
            val api = BuildmeshApi(server.url("/").toString())
            withContext(Dispatchers.Main) {
                val previous = StrictMode.getThreadPolicy()
                StrictMode.setThreadPolicy(StrictMode.ThreadPolicy.Builder().detectNetwork().penaltyDeathOnNetwork().build())
                try { assertEquals(64_000, api.get("/api/agents/7/diff").obj().text("diff").length) }
                finally { StrictMode.setThreadPolicy(previous) }
            }
            api.close()
        }
    }

    @Test fun repeatedRefreshDoesNotCancelSlowSnapshotAndBackgroundPreventsStaleCommit() = runBlocking {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            val started = CountDownLatch(1)
            val release = CountDownLatch(1)
            val completed = CountDownLatch(1)
            val secondRelease = CountDownLatch(1)
            val secondStarted = CountDownLatch(1)
            val secondCompleted = CountDownLatch(1)
            var nodeReads = 0
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = when (request.path) {
                    "/api/session" -> MockResponse().setResponseCode(204)
                    "/api/meshes", "/api/providers" -> MockResponse().setBody("[]")
                    "/api/ws-ticket" -> MockResponse().setResponseCode(503)
                    "/api/nodes" -> {
                        nodeReads++
                        val first = nodeReads == 1
                        if (first) { started.countDown(); check(release.await(10, TimeUnit.SECONDS)) }
                        else { secondStarted.countDown(); check(secondRelease.await(10, TimeUnit.SECONDS)); secondCompleted.countDown() }
                        completed.countDown()
                        MockResponse().setBody("[{\"id\":7,\"name\":\"${if (first) "slow snapshot" else "later snapshot"}\",\"status\":\"running\"}]")
                    }
                    else -> MockResponse().setResponseCode(404)
                }
            }
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            val vm = withContext(Dispatchers.Main) { BuildmeshViewModel(app).also { it.resume() } }
            try {
                assertTrue(started.await(5, TimeUnit.SECONDS))
                val firstVisible = async(start = CoroutineStart.UNDISPATCHED) { vm.state.first { it.nodes.isNotEmpty() }.nodes.single().text("name") }
                // Multiple refresh requests while the response is controlled must coalesce.
                withContext(Dispatchers.Main) { repeat(3) { vm.refresh() } }
                release.countDown()
                assertTrue(completed.await(5, TimeUnit.SECONDS))
                assertEquals("slow snapshot", withTimeout(5000) { firstVisible.await() })
                assertTrue(secondStarted.await(5, TimeUnit.SECONDS))
                withContext(Dispatchers.Main) { vm.pause() }
                secondRelease.countDown()
                assertTrue(secondCompleted.await(5, TimeUnit.SECONDS))
                withContext(Dispatchers.Main) { assertEquals("slow snapshot", vm.state.value.nodes.single().text("name")) }
                withContext(Dispatchers.Main) { vm.forget() }
                assertFalse(vm.state.value.paired)
                assertTrue(vm.state.value.nodes.isEmpty())
                assertNull(store.read())
            } finally {
                release.countDown()
                secondRelease.countDown()
                withContext(Dispatchers.Main) { vm.forget() }
                store.clear()
            }
        }
    }
}
