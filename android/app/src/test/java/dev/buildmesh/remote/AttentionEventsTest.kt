package dev.buildmesh.remote

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class AttentionEventsTest {
    @Test fun aLifecycleFrameCarriesTheNodeAndItsReason() {
        val event = parseFeedEvent(
            """{"type":"agent-lifecycle","session_id":42,"kind":"question_requested","status":"awaiting_input","message":"Which branch?"}"""
        )
        assertEquals(FeedEvent.Lifecycle(42, "question_requested", "awaiting_input", "Which branch?"), event)
        val lifecycle = event as FeedEvent.Lifecycle
        assertTrue(lifecycle.needsPerson)
        assertEquals("Needs an answer", lifecycle.reason)
    }

    @Test fun onlyAwaitingInputMeansTheNodeNeedsAPerson() {
        assertTrue(FeedEvent.Lifecycle(1, "input_required", "awaiting_input", "").needsPerson)
        assertFalse(FeedEvent.Lifecycle(1, "permission_requested", "ready", "").needsPerson)
        assertFalse(FeedEvent.Lifecycle(1, "background_running", "running", "").needsPerson)
        assertFalse(FeedEvent.Lifecycle(1, "turn_completed", "ready", "").needsPerson)
    }

    @Test fun theReasonIsTheKindTheNodeReported() {
        assertEquals("Needs an answer", attentionReason("question_requested"))
        assertEquals("Needs permission", attentionReason("permission_requested"))
        assertEquals("Needs attention", attentionReason("input_required"))
        assertEquals("Needs attention", attentionReason(""))
    }

    @Test fun aClearedFrameNamesTheNodeItClears() {
        assertEquals(FeedEvent.Cleared(9), parseFeedEvent("""{"type":"attention-cleared","session_id":9}"""))
    }

    @Test fun anUnknownOrMalformedFrameIsStillReported() {
        assertEquals(FeedEvent.Other("circuit-run-updated"), parseFeedEvent("""{"type":"circuit-run-updated","run_id":3}"""))
        assertEquals(FeedEvent.Other(""), parseFeedEvent("not json"))
        assertEquals(FeedEvent.Other(""), parseFeedEvent("[1,2,3]"))
    }

    @Test fun eachNodeHasItsOwnNotificationId() {
        assertEquals(attentionNotificationId(7), attentionNotificationId(7))
        assertNotEquals(attentionNotificationId(7), attentionNotificationId(8))
        assertNotEquals(attentionNotificationId(7), AgentNotifications.WATCH_NOTIFICATION_ID)
    }

    @Test fun aNodeLinkNamesTheNodeItOpens() {
        assertEquals(42L, nodeIdFromLink("buildmesh://node/42"))
        assertEquals(42L, nodeIdFromLink("buildmesh://node/42/"))
        assertEquals(42L, nodeIdFromLink("buildmesh://node/42?from=notice"))
    }

    @Test fun aLinkThatNamesNoNodeOpensNothing() {
        assertEquals(0L, nodeIdFromLink("buildmesh://pair?url=https://desktop:1992/"))
        assertEquals(0L, nodeIdFromLink("buildmesh://node/"))
        assertEquals(0L, nodeIdFromLink("buildmesh://node/not-a-number"))
        assertEquals(0L, nodeIdFromLink("https://example.test/node/42"))
    }

    @Test fun aNodeRowReadsTheSameConclusionTheEventCarries() {
        val node = buildJsonObject {
            put("id", 42)
            put("status", "awaiting_input")
            putJsonObject("lifecycle") {
                put("kind", "permission_requested")
                put("status", "awaiting_input")
                put("message", "Run Bash?")
            }
        }
        val attention = nodeAttention(node)
        assertEquals(42, attention.nodeId)
        assertTrue(attention.needsPerson)
        assertEquals("Needs permission", attention.reason)
        assertEquals("Run Bash?", attention.message)
    }

    @Test fun aStaleLifecycleDoesNotSpeakForTheNode() {
        val node = buildJsonObject {
            put("id", 42)
            put("status", "running")
            putJsonObject("lifecycle") {
                put("kind", "question_requested")
                put("status", "awaiting_input")
            }
        }
        val attention = nodeAttention(node)
        assertFalse(attention.needsPerson)
        assertEquals("Needs attention", attention.reason)
    }
}
