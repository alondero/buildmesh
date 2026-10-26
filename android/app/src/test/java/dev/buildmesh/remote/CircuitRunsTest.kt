package dev.buildmesh.remote

import dev.buildmesh.remote.wire.*
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class CircuitRunsTest {
    @Test fun reviewerPickerUsesBackendLaunchOrderAndExcludesUnavailableOrUnsupportedRows() {
        val providers = Json.parseToJsonElement("""[
            {"id":"codex","harness_id":"codex","capabilities":{"supports_passive_turn_watcher":true}},
            {"id":"launch/reviewer","harness_id":"claude","is_proxied":true,"configuration":{"id":"launch/reviewer","name":"Careful reviewer"},"capabilities":{"requires_attention_hook":true}},
            {"id":"claude:proxy","harness_id":"claude","is_proxied":true,"capabilities":{"requires_attention_hook":true}},
            {"id":"launch/offline","harness_id":"codex","unavailable_reason":"Route disabled","capabilities":{"supports_passive_turn_watcher":true}},
            {"id":"freebuff","harness_id":"freebuff","capabilities":{"requires_attention_hook":false,"supports_passive_turn_watcher":false}},
            {"id":"terminal","harness_id":"terminal","capabilities":{"supports_passive_turn_watcher":true}}
        ]""").objects()
        assertEquals(listOf("codex", "launch/reviewer"), reviewerLaunchOptions(providers).map { it.text("id") })
    }

    @Test fun builtInReviewSendsTheLaunchIdentityAndSavedCircuitsKeepTheirOwnReviewer() {
        assertEquals(Json.parseToJsonElement("""{"circuit_id":null,"max_rounds":4,"reviewer_provider":"launch/reviewer"}"""), startReviewRequest(null, 4, "launch/reviewer").toJson())
        assertEquals(Json.parseToJsonElement("""{"circuit_id":null,"max_rounds":3}"""), startReviewRequest(null, 3, "").toJson())
        assertEquals(Json.parseToJsonElement("""{"circuit_id":9,"max_rounds":3}"""), startReviewRequest(9, 4, "launch/reviewer").toJson())
    }

    @Test fun ownershipApiPreservesPairingAndOnlyAnAbsentRouteMeansNoSupport() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("""[{"node_id":18,"run_id":7,"circuit_id":3,"circuit_name":"Review","state":"running","parent_node_id":17}]"""))
            server.enqueue(MockResponse().setResponseCode(404))
            server.enqueue(MockResponse().setBody("""{"error":"not found"}"""))
            server.enqueue(MockResponse().setResponseCode(403).setBody("""{"error":"Refused"}"""))
            server.enqueue(MockResponse().setResponseCode(500).setBody("""{"error":"Read failed"}"""))
            server.enqueue(MockResponse().setBody("""[{"node_id":18}]"""))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                assertEquals(17L, api.circuitOwnerships().single().parent_node_id)
                assertEquals("/api/circuit-agent-ownerships", server.takeRequest().path)
                assertTrue(api.circuitOwnerships().isEmpty())
                assertTrue(api.circuitOwnerships().isEmpty())
                for (status in listOf(403, 500)) {
                    try { api.circuitOwnerships(); fail("Expected $status") }
                    catch (e: ApiException) { assertEquals(status, e.status) }
                }
                try { api.circuitOwnerships(); fail("Malformed ownership must be rejected") }
                catch (_: NoSuchElementException) { /* Generated decoder rejects missing fields. */ }
            } finally { api.close() }
        }
    }

    private val snapshot = """{"node_id":17,"run_id":7,"blueprint_name":"Review Blueprint","state":"running","step":{"node_id":"gate","role":"Approval gate","attempt":1,"status":"blocked"},"review":{"round":2,"max_rounds":3,"verdict":"changes_requested","summary":"Fix the retry loop."},"needs_you":[{"kind":"approval","step":{"node_id":"gate","role":"Approval gate","attempt":1,"status":"blocked"},"detail":null,"actions":[],"starts_next_review":false,"ends_review":false,"review_again":false,"agent_node_id":null}],"revision":12}"""
    private fun run() = NodeCircuitRun.fromJson(Json.parseToJsonElement(snapshot))

    @Test fun generatedWireReadsNullableAndOptionalFieldsAndHumanisesTheRun() {
        val run = run()
        assertNull(run.recovery)
        assertEquals("Fix the retry loop.", run.review?.summary)
        assertEquals("Needs you: Review Blueprint · Round 2 of 3 · Changes requested · Approval gate: Needs approval", circuitStatus(run))
        assertTrue(circuitNeedsAttention(buildJsonObject { put("id", 17); put("status", "running") }, listOf(run)))
        assertFalse(circuitNeedsAttention(buildJsonObject { put("id", 18); put("status", "running") }, listOf(run)))
        assertFalse(circuitNeedsAttention(buildJsonObject { put("id", 17); put("status", "running") }, listOf(run.copy(needs_you = emptyList()))))
    }

    @Test fun confirmationIsBoundToTheRunStepAttemptRevisionAndOfferedActions() {
        val run = run()
        val decision = CircuitDecision(7, 12, "approve", "gate", 1)
        assertTrue(decisionOffered(decision, run))
        assertFalse(decisionOffered(decision, run.copy(run_id = 8)))
        assertFalse(decisionOffered(decision, run.copy(revision = 13)))
        assertFalse(decisionOffered(decision, run.copy(needs_you = emptyList())))
        assertFalse(decisionOffered(decision.copy(attempt = 2), run))
        assertFalse(decisionOffered(decision.copy(kind = "recover", action = "continue"), run))
    }

    @Test fun circuitApiReadsTheGeneratedSnapshotAndPostsTheLiteralGateId() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("[$snapshot]"))
            server.enqueue(MockResponse().setBody("""{"ok":true,"run_id":7}"""))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                assertEquals(7L, api.circuitRuns().single().run_id)
                assertEquals("/api/circuit-runs", server.takeRequest().path)
                api.actOnCircuit(CircuitDecision(7, 12, "approve", "gate", 1), "")
                val request = server.takeRequest()
                assertEquals("/api/circuit-runs/7/approve", request.path)
                assertEquals("""{"node_id":"gate"}""", request.body.readUtf8())
            } finally { api.close() }
        }
    }

    @Test fun generatedOutcomeAndRecoveryBodiesPreserveRevisionAndReason() = runBlocking {
        MockWebServer().use { server ->
            repeat(2) { server.enqueue(MockResponse().setBody("""{"ok":true,"run_id":7}""")) }
            val api = BuildmeshApi(server.url("/").toString())
            try {
                api.actOnCircuit(CircuitDecision(7, 12, "outcome", "verdict", 2, "review_fail"), "Tests fail")
                val outcome = server.takeRequest()
                assertEquals("/api/circuit-runs/7/outcome", outcome.path)
                assertEquals(Json.parseToJsonElement("""{"run_id":7,"node_id":"verdict","attempt":2,"expected_revision":12,"action":"review_fail","reason":"Tests fail"}"""), Json.parseToJsonElement(outcome.body.readUtf8()))
                api.actOnCircuit(CircuitDecision(7, 13, "recover", "publish", 1, "continue"), "Pushed by hand")
                val recovery = server.takeRequest()
                assertEquals("/api/circuit-runs/7/recover", recovery.path)
                assertEquals("Pushed by hand", Json.parseToJsonElement(recovery.body.readUtf8()).jsonObject["reason"]?.jsonPrimitive?.content)
            } finally { api.close() }
        }
    }

    @Test fun olderDesktopHasNoCircuitSupportAndARefusalKeepsPairing() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("""{"error":"not found"}"""))
            server.enqueue(MockResponse().setResponseCode(409).setBody("""{"error":"The run changed"}"""))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                assertTrue(api.circuitRuns().isEmpty())
                try { api.actOnCircuit(CircuitDecision(7, 12, "cancel"), ""); fail("refusal expected") }
                catch (e: ApiException) { assertFalse(e.sessionRejected); assertEquals("The run changed", e.message) }
            } finally { api.close() }
        }
    }
}
