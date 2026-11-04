package dev.buildmesh.remote

import org.junit.Assert.*
import org.junit.Test

/** The phone's Node Glance reads the desktop's canonical glance body. */
class NodeGlanceTest {
    private val running = nodeGlance(MobileWireFixtures.obj("node_glance"))
    private val unsupported = nodeGlance(MobileWireFixtures.obj("node_glance_unsupported"))

    @Test fun readsTheTranscriptLayerFromTheDesktopBody() {
        assertEquals("running", running.status)
        assertEquals("Fix the login redirect", running.lastPrompt)
        assertEquals("The redirect drops the query string; fixing it now.", running.lastMessage)
        assertTrue(running.messageAfterPrompt)
        assertEquals(listOf("Edit · src/auth/redirect.ts", "Bash · npm test"), running.activity.map { it.line })
        assertEquals(14L, running.toolCallsSincePrompt)
        assertNull(running.unavailable)
    }

    @Test fun aRunningNodesLatestToolCallIsCurrentWorkAndOtherwiseHistory() {
        assertEquals("Now: Bash · npm test", glanceHeadline(running))
        assertEquals("Last: Bash · npm test", glanceHeadline(running.copy(status = "ready")))
        assertNull(glanceHeadline(unsupported))
        assertEquals("14 tool calls since the prompt", glanceToolCount(running))
        assertEquals("1 tool call since the prompt", glanceToolCount(running.copy(toolCallsSincePrompt = 1)))
        assertNull(glanceToolCount(unsupported))
    }

    @Test fun timesAreMeasuredOnTheDesktopClockPlusTheTimeHeld() {
        // Observed 12m after the status change, 2h into the session and 8s
        // after the last transcript write; the phone has held it 30s.
        assertEquals(
            "Running for 12m · session 2h · last activity 38s ago",
            glanceTimingLine(running, "Running", 30_000),
        )
        // No session start yet: the session age falls back to the node's age.
        assertEquals("Ready for 5m · session 2h 6m", glanceTimingLine(unsupported, "Ready", 0))
    }

    @Test fun anUnreadableTranscriptNamesItsReason() {
        assertNull(unsupported.lastMessage)
        assertTrue(unsupported.activity.isEmpty())
        assertEquals("unsupported", unsupported.unavailable)
        assertEquals("This harness doesn't keep a transcript Buildmesh can read.", glanceUnavailableCopy(unsupported.unavailable!!))
    }

    @Test fun elapsedTimeIsCompact() {
        assertEquals("0s", formatElapsed(-5))
        assertEquals("45s", formatElapsed(45_000))
        assertEquals("12m", formatElapsed(12 * 60_000L))
        assertEquals("3h", formatElapsed(3 * 3_600_000L))
        assertEquals("3h 5m", formatElapsed(3 * 3_600_000L + 5 * 60_000L))
        assertEquals("2d 4h", formatElapsed(52 * 3_600_000L))
    }
}
