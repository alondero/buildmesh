package dev.buildmesh.remote

import dev.buildmesh.remote.wire.CircuitAgentOwnership
import dev.buildmesh.remote.wire.NodeCircuitRun
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class WorkActivitiesTest {
    private fun node(id: Long, position: Long = id, mesh: Long = 1, status: String = "idle", name: String = "Agent $id") =
        buildJsonObject {
            put("id", id); put("position", position); put("mesh_id", mesh); put("status", status); put("name", name); put("branch", "branch-$id")
        }
    private fun owner(id: Long, parent: Long?, state: String = "running") =
        CircuitAgentOwnership(id, 7, 3, "Review", state, parent)
    private fun circuitRun(nodeId: Long) = NodeCircuitRun.fromJson(Json.parseToJsonElement(
        """{"node_id":$nodeId,"run_id":7,"blueprint_name":"Review","state":"running","step":null,"review":null,"needs_you":[{"kind":"approval","step":{"node_id":"gate","role":"Approval gate","attempt":1,"status":"blocked"},"detail":null,"actions":[],"starts_next_review":false,"ends_review":false,"review_again":false,"agent_node_id":null}],"revision":1}"""))

    @Test fun reviewersFollowTheirRepresentativeEvenWhenTheirPositionIsEarlier() {
        val nodes = listOf(node(12, 1), node(11, 3), node(20, 2), node(13, 4))
        val activities = workActivities(nodes, listOf(owner(12, 11), owner(13, 12)))
        assertEquals(listOf(20L, 11L), activities.map { it.root.number("id") })
        assertEquals(listOf(11L, 12L, 13L), activities.last().members.map { it.number("id") })
    }

    @Test fun invalidLineageAndArchivedParentsLeaveAgentsReachableWithoutCrossingMeshes() {
        val nodes = listOf(node(1, status = "archived"), node(2), node(3), node(4, mesh = 2), node(5), node(6), node(7))
        val activities = workActivities(nodes, listOf(owner(2, 1), owner(3, 999), owner(4, 2), owner(5, 6), owner(6, 5), owner(7, 5)))
        assertEquals(listOf(2L, 3L, 4L, 5L, 6L, 7L), activities.map { it.root.number("id") })
        assertTrue(activities.all { it.members.size == 1 })
    }

    @Test fun searchingOrFilteringAReviewerRetainsItsWholePairAndRespectsMeshScope() {
        val state = RemoteState(nodes = listOf(node(1), node(2, status = "awaiting_input", name = "Reviewer"), node(3, mesh = 2)),
            circuitOwnerships = listOf(owner(2, 1)))
        assertEquals(listOf(1L, 2L), matchingWorkActivities(state, 0, "Reviewer", true).single().members.map { it.number("id") })
        assertEquals(1L, matchingWorkActivities(state, 1, "branch-2", false).single().root.number("id"))
        assertTrue(matchingWorkActivities(state, 2, "Reviewer", false).isEmpty())
        assertTrue(matchingWorkActivities(state, 0, "Agent 1", true).isEmpty())
        assertEquals(2, matchingWorkActivities(state, 0, "", false).size)
    }

    @Test fun aWaitingAgentIsTheFirstActivityOfItsMeshGroup() {
        val state = RemoteState(nodes = listOf(
            node(1, position = 1, status = "running"),
            node(2, position = 2, status = "running"),
            node(3, position = 3, status = "awaiting_input")))
        assertEquals(listOf(3L, 1L, 2L), matchingWorkActivities(state, 0, "", false).map { it.root.number("id") })
    }

    @Test fun aFailedAgentOutranksACircuitGateAndBothOutrankAwaitingInput() {
        val state = RemoteState(nodes = listOf(
            node(1, position = 1, status = "running"),
            node(2, position = 2, status = "awaiting_input"),
            node(3, position = 3, status = "running"),
            node(4, position = 4, status = "error"),
            node(5, position = 5, status = "lost")),
            circuitRuns = listOf(circuitRun(3)))
        // Error and lost tie, so they keep the desktop's saved order between them.
        assertEquals(listOf(4L, 5L, 3L, 2L, 1L), matchingWorkActivities(state, 0, "", false).map { it.root.number("id") })
    }

    @Test fun aCircuitBracketMovesAsAUnitAndKeepsItsRepresentativeFirst() {
        val state = RemoteState(nodes = listOf(
            node(20, position = 1, status = "running"),
            node(11, position = 2, status = "running"),
            node(12, position = 3, status = "running"),
            node(13, position = 4, status = "awaiting_input")),
            circuitOwnerships = listOf(owner(12, 11), owner(13, 11)))
        val activities = matchingWorkActivities(state, 0, "", false)
        assertEquals(listOf(11L, 20L), activities.map { it.root.number("id") })
        assertEquals(listOf(11L, 12L, 13L), activities.first().members.map { it.number("id") })
    }

    @Test fun anActivityNeverLeavesItsMeshGroupToJumpAheadOfAnotherMesh() {
        val state = RemoteState(nodes = listOf(
            node(1, position = 1, mesh = 1, status = "running"),
            node(2, position = 2, mesh = 1, status = "running"),
            node(3, position = 3, mesh = 2, status = "awaiting_input")))
        assertEquals(listOf(1L, 2L, 3L), matchingWorkActivities(state, 0, "", false).map { it.root.number("id") })
        assertEquals(listOf(3L), matchingWorkActivities(state, 2, "", false).map { it.root.number("id") })
    }

    @Test fun statusShapesAndCircuitOrbitsDistinguishTheDesktopStates() {
        assertEquals(listOf("solid", "ring", "dashed", "dashed", "target", "cross", "slash", "half", "target", "target", "thin"),
            listOf("running", "idle", "pending", "spawning", "awaiting_input", "error", "lost", "suspended", "ready", "completed", "archived").map(::workStatusShape))
        assertEquals("comet", workCircuitOrbit(node(1, status = "running"), owner(1, null)))
        assertEquals("half", workCircuitOrbit(node(1), owner(1, null)))
        assertEquals("half", workCircuitOrbit(node(1, status = "running"), owner(1, null, "paused")))
        assertEquals("dashed", workCircuitOrbit(node(1), owner(1, null, "failed")))
        assertEquals("dashed", workCircuitOrbit(node(1), owner(1, null, "unknown")))
        assertEquals("closed", workCircuitOrbit(node(1), owner(1, null, "completed")))
        assertNull(workCircuitOrbit(node(1), owner(1, null, "cancelled")))
        assertNull(workCircuitOrbit(node(1), null))
    }

    @Test fun aStatusThisBuildDoesNotKnowIsAnExplicitGapRatherThanAMutedDot() {
        // The vocabulary is generated from STATUS_CONFIG (src/lib/status.ts),
        // so a status the desktop adds that this build has not caught up with
        // is named and marked — it does not fall back to a grey circle with a
        // generic label that reads like a healthy state.
        assertNull(MeshStatusTable.find("brand_new_status"))
        assertEquals("Unknown status", MeshStatusTable.resolve("brand_new_status").label)
        assertEquals(MeshStatusTable.Unknown.hex, MeshStatusTable.resolve("brand_new_status").hex)
        assertEquals("cross", workStatusShape("brand_new_status"))
        // A status the vocabulary does know keeps its own label, colour and shape.
        assertEquals("Running", MeshStatusTable.resolve("running").label)
        assertEquals("solid", workStatusShape("running"))
        assertEquals("Needs attention", MeshStatusTable.resolve("awaiting_input").label)
        assertEquals("target", workStatusShape("awaiting_input"))
        // The pulsing set is the one STATUS_CONFIG marks, not a hand-written list.
        assertEquals(setOf("pending", "spawning", "awaiting_input"), MeshStatusTable.pulsing)
    }
}
