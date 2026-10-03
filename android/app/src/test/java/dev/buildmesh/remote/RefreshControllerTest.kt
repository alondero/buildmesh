package dev.buildmesh.remote

import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class RefreshControllerTest {
    @Test fun pollingCoalescesWhileSlowReadStillPublishes() = runTest {
        val reads = listOf(CompletableDeferred<String>(), CompletableDeferred<String>())
        var started = 0
        val visible = mutableListOf<String>()
        val errors = mutableListOf<Exception>()
        val refresh = RefreshController(this, { reads[started++].await() }, visible::add, errors::add, {})
        refresh.refresh(); runCurrent()
        repeat(3) { refresh.refresh(); advanceTimeBy(5000); runCurrent() }
        assertEquals(1, started)
        reads[0].complete("slow snapshot"); runCurrent()
        assertEquals(listOf("slow snapshot"), visible)
        assertEquals(2, started)
        reads[1].complete("queued snapshot"); runCurrent()
        assertEquals(listOf("slow snapshot", "queued snapshot"), visible)
        assertTrue(errors.isEmpty())
        refresh.stop()
    }

    @Test fun backgroundThenResumeRejectsLateOldCleanupAndKeepsNewReadOwned() = runTest {
        val reads = List(3) { CompletableDeferred<String>() }
        var started = 0
        val visible = mutableListOf<String>()
        var busy = false
        val refresh = RefreshController(this, {
            val index = started++
            if (index == 0) withContext(NonCancellable) { reads[index].await() } else reads[index].await()
        }, visible::add, { throw it }, { busy = it })
        refresh.refresh(); runCurrent()
        refresh.stop(); refresh.refresh(); runCurrent()
        refresh.refresh()
        reads[0].complete("old background response"); runCurrent()
        assertEquals(2, started)
        assertTrue(busy)
        assertTrue(visible.isEmpty())
        reads[1].complete("resumed snapshot"); runCurrent()
        assertEquals(listOf("resumed snapshot"), visible)
        assertEquals(3, started)
        reads[2].complete("coalesced snapshot"); runCurrent()
        assertEquals(listOf("resumed snapshot", "coalesced snapshot"), visible)
        assertFalse(busy)
    }

    @Test fun eventOrMutationInvalidatesOldSnapshotAndRejectionCanRecover() = runTest {
        val reads = List(3) { CompletableDeferred<String>() }
        var started = 0
        val visible = mutableListOf<String>()
        val errors = mutableListOf<Exception>()
        val refresh = RefreshController(this, { reads[started++].await() }, visible::add, errors::add, {})
        refresh.refresh(); runCurrent()
        refresh.invalidate(); refresh.refresh()
        reads[0].complete("before newly accepted node"); runCurrent()
        assertTrue(visible.isEmpty())
        reads[1].completeExceptionally(java.io.IOException("offline")); runCurrent()
        assertEquals("offline", errors.single().message)
        refresh.refresh(); runCurrent()
        reads[2].complete("recovered with new node"); runCurrent()
        assertEquals(listOf("recovered with new node"), visible)
    }
}
