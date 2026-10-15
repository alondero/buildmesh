package dev.buildmesh.remote

import java.security.MessageDigest
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import org.junit.Assert.*
import org.junit.Test

class BuildmeshApiTest {
    @Test fun sequentialReadsAndMutationsUseSeparateServerConnections() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"name\":\"read\"}"))
            server.enqueue(MockResponse().setBody("{\"name\":\"mutation\"}"))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                assertEquals("read", api.get("/api/nodes").obj().text("name"))
                assertEquals("mutation", api.post("/api/nodes/7/input", inputBody("yes\r")).obj().text("name"))
                val first = server.takeRequest(5, TimeUnit.SECONDS)!!
                val second = server.takeRequest(5, TimeUnit.SECONDS)!!
                assertEquals(0, first.sequenceNumber)
                assertEquals(0, second.sequenceNumber)
                assertEquals("/api/nodes/7/input", second.path)
                assertEquals(2, server.requestCount)
            } finally { api.close() }
        }
    }

    @Test fun pairingCookieSurvivesClientRecreationAndNeverAppearsInUrls() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(204).addHeader("Set-Cookie", "bm_session=device-secret; Path=/; HttpOnly; Max-Age=34560000; SameSite=Strict"))
            server.enqueue(MockResponse().setResponseCode(204).addHeader("Set-Cookie", "bm_session=device-secret; Path=/; HttpOnly; Max-Age=34560000; SameSite=Strict"))
            server.enqueue(MockResponse().setBody("[{\"id\":7,\"name\":\"remote agent\"}]"))
            val origin = server.url("/").toString().trimEnd('/')
            val first = BuildmeshApi(origin)
            first.pair("one-time-invitation")
            val pair = server.takeRequest()
            assertEquals("/api/pair", pair.path)
            assertEquals("Bearer one-time-invitation", pair.getHeader("Authorization"))
            assertNull(pair.getHeader("Cookie"))
            val restored = BuildmeshApi(origin, initialCookie = first.session().cookie)
            restored.restore()
            val refresh = server.takeRequest()
            assertEquals("/api/session", refresh.path)
            assertEquals("bm_session=device-secret", refresh.getHeader("Cookie"))
            assertNull(refresh.getHeader("Authorization"))
            assertEquals(7L, restored.get("/api/nodes").objects().single().number("id"))
            assertEquals("bm_session=device-secret", server.takeRequest().getHeader("Cookie"))
            first.close(); restored.close()
        }
    }

    @Test fun redirectCannotForwardTheInvitation() = runBlocking {
        MockWebServer().use { server ->
            MockWebServer().use { attacker ->
                server.enqueue(MockResponse().setResponseCode(302).addHeader("Location", attacker.url("/collect")))
                try { BuildmeshApi(server.url("/").toString()).pair("private-invitation"); fail("Redirect accepted") }
                catch (e: ApiException) { assertEquals(302, e.status) }
                assertNull(attacker.takeRequest(100, TimeUnit.MILLISECONDS))
                assertEquals("/api/pair", server.takeRequest().path)
            }
        }
    }

    @Test fun revokedSessionProducesAuthenticationFailure() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(401).setBody("{\"error\":\"revoked\"}"))
            try { BuildmeshApi(server.url("/").toString(), initialCookie = "bm_session=old; Path=/").get("/api/nodes"); fail("Revoked session accepted") }
            catch (e: ApiException) { assertTrue(e.unauthorized); assertEquals("revoked", e.message) }
        }
    }

    @Test fun ticketsAreBoundToNodeAndRateLimitRetriesOnlyOnce() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(429).addHeader("Retry-After", "0"))
            server.enqueue(MockResponse().setBody("{\"ticket\":\"short-lived\"}"))
            val api = BuildmeshApi(server.url("/").toString())
            val ticket = api.ticket("terminal", 42)
            repeat(2) {
                val request = server.takeRequest()
                assertEquals("/api/ws-ticket", request.path)
                assertEquals("{\"surface\":\"terminal\",\"node_id\":42}", request.body.readUtf8())
            }
            assertEquals("/ws/terminal/42?ticket=short-lived", api.socketRequest("terminal", ticket, 42).url.encodedPath + "?" + api.socketRequest("terminal", ticket, 42).url.encodedQuery)
            server.enqueue(MockResponse().setResponseCode(429).addHeader("Retry-After", "0"))
            server.enqueue(MockResponse().setResponseCode(429))
            try { api.ticket("events"); fail("Persistent rate limit accepted") }
            catch (e: ApiException) { assertEquals(429, e.status) }
            assertEquals(4, server.requestCount)
        }
    }

    @Test fun mutationsSendLiteralWirePayloadAndPartialResumeIsNotSuccess() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{}"))
            val api = BuildmeshApi(server.url("/").toString())
            api.sendKeys(8, "yes\r")
            val input = server.takeRequest()
            assertEquals("/api/nodes/8/input", input.path)
            assertEquals("{\"seq\":\"yes\\r\"}", input.body.readUtf8())
            server.enqueue(MockResponse().setResponseCode(207).setBody("{\"node\":{\"id\":19},\"spawn_error\":\"harness unavailable\"}"))
            try { api.post("/api/meshes/2/agent-nodes/import-and-resume", buildJsonObject { put("cli_session_id", "s") }); fail("Partial success accepted") }
            catch (e: ApiException) { assertEquals(207, e.status); assertTrue(e.message!!.contains("Node 19")) }
            assertEquals(2, server.requestCount)
        }
    }

    @Test fun rootPinAndHostnameAreBothRequiredBeforeSendingCredentials() = runBlocking {
        val root = HeldCertificate.Builder().certificateAuthority(1).commonName("Buildmesh root").build()
        val leaf = HeldCertificate.Builder().commonName("desktop").addSubjectAlternativeName("localhost").signedBy(root).build()
        val certificates = HandshakeCertificates.Builder().heldCertificate(leaf, root.certificate).build()
        MockWebServer().use { server ->
            server.useHttps(certificates.sslSocketFactory(), false)
            server.enqueue(MockResponse().setResponseCode(204).addHeader("Set-Cookie", "bm_session=tls-secret; Path=/; HttpOnly; Secure"))
            val bytes = root.certificate.encoded
            val hash = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
            assertEquals(root.certificate, verifiedRoot(bytes, hash))
            try { verifiedRoot(bytes, "00".repeat(32)); fail("Mismatched pin accepted") } catch (_: IllegalArgumentException) { }
            val client = BuildmeshApi(server.url("/").newBuilder().host("localhost").build().toString(), root = bytes)
            client.pair("tls-invitation")
            assertEquals("Bearer tls-invitation", server.takeRequest().getHeader("Authorization"))
            val wrongHost = server.url("/").newBuilder().host("127.0.0.1").build().toString()
            try { BuildmeshApi(wrongHost, root = bytes).pair("must-not-send"); fail("Hostname mismatch accepted") }
            catch (_: javax.net.ssl.SSLPeerUnverifiedException) { }
            assertEquals(1, server.requestCount)
        }
    }

    @Test fun invitationsRejectInsecureRemoteUrlsAndKeepSecretsInFragment() {
        val ticket = "a".repeat(64)
        val pin = "bb".repeat(32)
        val invitation = Invitation.parse("https://192.168.1.8:1992/#pair=$ticket&ca=$pin")
        assertEquals("https://192.168.1.8:1992", invitation.origin)
        assertEquals(ticket, invitation.ticket)
        assertEquals(pin, invitation.fingerprint)
        listOf("http://192.168.1.8/#pair=$ticket", "https://host/?token=$ticket", "https://user:pass@host/#pair=$ticket&ca=$pin", "https://host/#pair=$ticket", "file:///tmp/#pair=$ticket").forEach {
            try { Invitation.parse(it); fail("Invalid invitation accepted: ${it.substringBefore('#')}") } catch (_: IllegalArgumentException) { }
        }
        assertNull(Invitation.parse("http://127.0.0.1:2992/#pair=$ticket").fingerprint)
    }

    @Test fun replyLimitCountsEncodedUtf8AndLifecycleLabelsRequireMatchingStatus() {
        assertEquals("yes\r", inputBody("yes\r").text("seq"))
        try { inputBody("界".repeat(400)); fail("Oversized Unicode reply accepted") } catch (_: IllegalArgumentException) { }
        val node = Json.parseToJsonElement("{\"status\":\"running\",\"lifecycle\":{\"status\":\"awaiting_input\",\"kind\":\"question_requested\"}}").obj()
        assertEquals("Running", nodeLabel(node))
        assertEquals("Needs an answer", nodeLabel(JsonObject(node + ("status" to JsonPrimitive("awaiting_input")))))
    }

    // Issue #2331 review finding 1. `ensureSuccess` intercepts 207 for the
    // import-and-resume route, but create-and-start answers 207 with a typed
    // `outcome`. If 207 stayed unconditional, every `start_failed` create threw
    // an "imported" error and `readCreateNodeOutcome` was never reached, so the
    // committed node was stranded. These drive the real HTTP path so the
    // collision cannot come back unnoticed.
    @Test fun aCreateFailureSurvivesHttp207AndReachesTheOutcomeReader() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(207).setBody(
                """{"outcome":"start_failed","node":{"id":9,"mesh_id":1,"name":"idea"},"operation_key":"op-1","start_error":"claude CLI not found on PATH"}"""
            ))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val outcome = readCreateNodeOutcome(api.post("/api/nodes/create", createNodeBody(1, Json.parseToJsonElement("""{"id":"anthropic"}""").obj(), "Fix it", "op-1")).obj())
                assertTrue(outcome is CreateNodeOutcome.StartFailed)
                assertEquals(9L, (outcome as CreateNodeOutcome.StartFailed).node.number("id"))
                assertEquals("claude CLI not found on PATH", outcome.startError)
            } finally { api.close() }
        }
    }

    @Test fun theImportRouteKeepsItsOwn207Exception() = runBlocking {
        // The import shape has no `outcome`, so it must still throw — that is
        // what ResourceScreens catches to show the partial-import message.
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(207).setBody(
                """{"node":{"id":9,"mesh_id":1,"name":"idea"},"spawn_error":"worktree already exists"}"""
            ))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                api.post("/api/meshes/1/agent-nodes/import-and-resume", buildJsonObject { put("cli_session_id", "s-1") })
                fail("import 207 no longer raises its partial-import error")
            } catch (e: ApiException) {
                assertEquals(207, e.status)
                assertTrue("unexpected 207 message: ${e.message}", e.message.orEmpty().contains("worktree already exists"))
            } finally { api.close() }
        }
    }
}
