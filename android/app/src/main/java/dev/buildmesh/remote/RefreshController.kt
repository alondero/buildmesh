package dev.buildmesh.remote

import kotlinx.coroutines.*

/** Main-thread owner for coalesced reads; stop and invalidate fence every completion and cleanup. */
class RefreshController<T>(
    private val scope: CoroutineScope,
    private val read: suspend () -> T,
    private val publish: (T) -> Unit,
    private val onError: (Exception) -> Unit,
    private val onBusy: (Boolean) -> Unit,
) {
    private var job: Job? = null
    private var queued = false
    private var revision = 0
    private var owner = 0

    fun invalidate() { revision++ }

    fun refresh() {
        if (job?.isActive == true) { queued = true; return }
        val requestOwner = ++owner
        val requestRevision = revision
        onBusy(true)
        job = scope.launch(start = CoroutineStart.LAZY) {
            try {
                val result = read()
                if (requestOwner == owner && requestRevision == revision) publish(result)
            } catch (e: CancellationException) { throw e
            } catch (e: Exception) {
                if (requestOwner == owner && requestRevision == revision) onError(e)
            } finally {
                if (requestOwner == owner) {
                    job = null
                    if (queued) { queued = false; refresh() }
                    else onBusy(false)
                }
            }
        }
        job?.start()
    }

    fun stop() {
        owner++; revision++; queued = false
        job?.cancel(); job = null
        onBusy(false)
    }
}
