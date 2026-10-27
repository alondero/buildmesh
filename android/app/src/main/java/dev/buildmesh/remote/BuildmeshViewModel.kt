package dev.buildmesh.remote

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import androidx.core.content.edit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.security.MessageDigest
import java.util.Base64
import dev.buildmesh.remote.wire.NodeCircuitRun
import dev.buildmesh.remote.wire.CircuitAgentOwnership

private data class DashboardSnapshot(val meshes: List<JsonObject>, val nodes: List<JsonObject>, val providers: List<JsonObject>, val runs: List<NodeCircuitRun>?, val ownerships: List<CircuitAgentOwnership>?)

data class RemoteState(
    val restoring: Boolean = true,
    val paired: Boolean = false,
    val busy: Boolean = false,
    val refreshing: Boolean = false,
    /**
     * Whether the in-flight snapshot read is one the user asked for.
     *
     * The dashboard polls every five seconds; a read the user did not request
     * must not pulse the chrome, or every screen twitches while nothing is
     * happening. The first load still shows its own progress, which is what
     * `loaded` is for.
     */
    val manualRefresh: Boolean = false,
    /**
     * Whether a snapshot has been read at least once.
     *
     * `restoring` covers the credential; this covers the inventory. A screen
     * that needs to know whether a node is missing has to wait for this,
     * because an empty list before the first read means "not loaded yet", not
     * "no nodes".
     */
    val loaded: Boolean = false,
    val error: String = "",
    val notice: String = "",
    val origin: String = "",
    val draftNamespace: String = "",
    val meshes: List<JsonObject> = emptyList(),
    val nodes: List<JsonObject> = emptyList(),
    val providers: List<JsonObject> = emptyList(),
    val circuitRuns: List<NodeCircuitRun> = emptyList(),
    val circuitOwnerships: List<CircuitAgentOwnership> = emptyList(),
    val resourceRevision: Int = 0,
)

class BuildmeshViewModel(application: Application) : AndroidViewModel(application) {
    private val store = SessionStore(application)
    private val draftPrefs = application.getSharedPreferences("drafts", 0)
    private val mutableState = MutableStateFlow(RemoteState())
    val state = mutableState.asStateFlow()
    var api: BuildmeshApi? = null
        private set
    private var generation = 0
    private var polling: Job? = null
    private var actionJob: Job? = null
    private var foreground = false

    /**
     * The dashboard's share of the desktop's event feed.
     *
     * The feed is process-wide, so the background watch service and this
     * dashboard read the same socket rather than opening a second one. The
     * feed delivers on its own worker threads, and the refresher is a
     * main-thread owner, so the listener hands its callbacks to this
     * ViewModel's scope.
     */
    private val feedListener = ScopedFeedListener(
        scope = viewModelScope,
        opened = { refresh() },
        frame = {
            // Invalidate an in-flight snapshot before reading the new authoritative state.
            refresher.invalidate()
            refresh()
        },
        refused = { unauthorized() },
    )
    private val refresher = RefreshController(viewModelScope,
        read = {
            val client = api ?: throw CancellationException("Disconnected")
            coroutineScope {
                val meshes = async { client.get("/api/meshes").objects() }
                val nodes = async { client.get("/api/nodes").objects() }
                val providers = async { client.get("/api/providers").objects() }
                val runs = async {
                    try { client.circuitRuns() }
                    catch (e: CancellationException) { throw e }
                    catch (e: Exception) {
                        if (e is ApiException && e.sessionRejected) throw e
                        null
                    }
                }
                val ownerships = async {
                    try { client.circuitOwnerships() }
                    catch (e: CancellationException) { throw e }
                    catch (e: Exception) {
                        if (e is ApiException && e.sessionRejected) throw e
                        null
                    }
                }
                DashboardSnapshot(meshes.await(), nodes.await(), providers.await(), runs.await(), ownerships.await())
            }
        }, publish = { snapshot ->
            mutableState.update { it.copy(meshes = snapshot.meshes, nodes = snapshot.nodes, providers = snapshot.providers, circuitRuns = snapshot.runs ?: it.circuitRuns, circuitOwnerships = snapshot.ownerships ?: it.circuitOwnerships, error = "", loaded = true) }
        }, onError = ::report,
        onBusy = { busy -> mutableState.update { it.copy(refreshing = busy, manualRefresh = busy && it.manualRefresh) } },
    )

    init {
        viewModelScope.launch {
            val saved = withContext(Dispatchers.IO) { store.read() }
            if (saved != null) {
                try {
                    api = BuildmeshApi(saved.origin, saved.root, saved.cookie)
                    mutableState.update { it.copy(paired = true, origin = saved.origin, draftNamespace = draftNamespace(saved)) }
                    api!!.restore()
                    store.save(api!!.session())
                    startWatchIfEnabled()
                } catch (e: Exception) {
                    // `/api/session` is the credential boundary: it answers 401 or
                    // 403 for a credential it will not re-mint, so either one means
                    // this phone must pair again. A network failure says nothing
                    // about the credential and keeps it.
                    if (e is ApiException && e.credentialRefused) unauthorized()
                    else mutableState.update { it.copy(error = "Cannot reach the desktop. Check that Buildmesh is running and your network or VPN is connected.") }
                }
            }
            mutableState.update { it.copy(restoring = false) }
            if (foreground) resume()
        }
    }

    fun pair(url: String, fingerprint: String) {
        if (state.value.busy) return
        mutableState.update { it.copy(busy = true, error = "") }
        val owner = generation
        viewModelScope.launch {
            var candidate: BuildmeshApi? = null
            try {
                val invitation = Invitation.parse(url, fingerprint)
                val root = withContext(Dispatchers.IO) { if (invitation.fingerprint == null) null else downloadRoot(invitation) }
                ensureActive()
                if (owner != generation) return@launch
                candidate = BuildmeshApi(invitation.origin, root)
                candidate.pair(invitation.ticket)
                if (owner != generation) return@launch
                val session = candidate.session()
                store.save(session)
                // The feed's socket is bound to the credential the desktop
                // accepts, so a fresh pairing has to re-mint it.
                AppEventFeed.feed.reload()
                api?.close()
                api = BuildmeshApi(session.origin, session.root, session.cookie)
                startWatchIfEnabled()
                mutableState.value = RemoteState(restoring = false, paired = true, origin = session.origin, draftNamespace = draftNamespace(session))
                if (foreground) resume()
            } catch (e: CancellationException) { throw e
            } catch (e: Exception) {
                if (owner == generation) mutableState.update { it.copy(error = friendlyError(e)) }
            } finally {
                candidate?.close()
                if (owner == generation) mutableState.update { it.copy(busy = false) }
            }
        }
    }

    fun resume() {
        foreground = true
        // Without a paired desktop there is nothing to read and nothing to
        // listen to, so the feed stays closed.
        if (api == null) return
        if (state.value.restoring || polling?.isActive == true) return
        refresh()
        polling = viewModelScope.launch { while (isActive) { delay(5000); refresh() } }
        AppEventFeed.bind(getApplication<Application>())
        AppEventFeed.feed.attach(feedListener)
    }

    fun pause() {
        foreground = false
        polling?.cancel(); polling = null
        refresher.stop()
        AppEventFeed.feed.detach(feedListener)
        mutableState.update { it.copy(refreshing = false) }
    }

    /** Re-read the dashboard snapshot, reporting whether a read actually started. */
    fun refresh(): Boolean {
        if (api == null || !foreground || state.value.restoring) return false
        refresher.refresh()
        return true
    }

    fun acceptNode(node: JsonObject) {
        refresher.invalidate()
        mutableState.update { it.copy(nodes = it.nodes.filterNot { existing -> existing.number("id") == node.number("id") } + node) }
        refresh()
    }

    /**
     * A user-initiated refresh: the resource screens reload, the snapshot is
     * re-read, and because the user asked, the progress bar may say so.
     */
    fun retry() {
        mutableState.update {
            it.copy(resourceRevision = it.resourceRevision + 1, manualRefresh = api != null && foreground && !it.restoring)
        }
        refresh()
    }

    fun closeNode(nodeId: Long) {
        action { client ->
            client.post("/api/nodes/$nodeId/close", JsonObject(emptyMap()))
            refresher.invalidate()
            mutableState.update { it.copy(nodes = it.nodes.filterNot { node -> node.number("id") == nodeId },
                notice = "Agent closed. Worktree and files kept.") }
        }
    }

    /**
     * Run one mutating request, reporting whether it was queued.
     *
     * A refused action never runs its block, so a caller that disables controls
     * optimistically needs to know the difference between "sending" and "this
     * request was dropped" — otherwise a declined tap leaves a permanently
     * dead control behind.
     *
     * `onFailed` is the other half of that contract: it fires when the block
     * did not finish, so a caller waiting for an acceptance callback (the
     * attention deck's reply chips) can release its in-flight marker instead
     * of showing "Sending…" forever. The desktop's own acceptance still
     * arrives through the block itself.
     */
    fun action(onFailed: (() -> Unit)? = null, block: suspend (BuildmeshApi) -> Unit): Boolean {
        val client = api ?: return false
        if (state.value.busy) return false
        val owner = generation
        mutableState.update { it.copy(busy = true, error = "", notice = "") }
        refresher.invalidate()
        actionJob = viewModelScope.launch(start = CoroutineStart.LAZY) {
            // Only a block that ran to completion has reported its own outcome;
            // anything else is a failure the caller still believes is in flight.
            var delivered = false
            try {
                block(client)
                delivered = true
                if (owner == generation) { refresher.invalidate(); refresh() }
            } catch (e: CancellationException) {
                if (!delivered) onFailed?.invoke()
                throw e
            } catch (e: Exception) {
                if (!delivered) onFailed?.invoke()
                if (owner == generation) {
                    // A data route that answers 403 refused this request; the
                    // device is still paired (issue #2317).
                    if (e is ApiException && e.sessionRejected) unauthorized()
                    else mutableState.update { it.copy(error = friendlyError(e)) }
                }
            } finally { if (owner == generation) {
                actionJob = null
                mutableState.update { it.copy(busy = false) }
            } }
        }
        actionJob?.start()
        return true
    }

    fun notice(message: String) { mutableState.update { it.copy(notice = message) } }
    fun report(e: Exception) {
        // Polling reads are data routes too: a 403 belongs in the error slot.
        if (e is ApiException && e.sessionRejected) unauthorized()
        else mutableState.update { it.copy(error = friendlyError(e)) }
    }
    fun dismissError() { mutableState.update { it.copy(error = "", notice = "") } }
    fun draft(key: String) = draftPrefs.getString(scopedDraftKey(key), "").orEmpty()
    fun saveDraft(key: String, value: String) { draftPrefs.edit { putString(scopedDraftKey(key), value) } }

    private fun scopedDraftKey(key: String) = "${state.value.draftNamespace}:$key"

    fun forget() {
        generation++
        actionJob?.cancel(); actionJob = null
        pause()
        api?.close(); api = null
        store.clear()
        // Nothing on this phone may keep using the credential that was just
        // withdrawn, so the feed is rebuilt against an empty store.
        AppEventFeed.feed.reload()
        // A watch with no desktop to watch is a phone claiming to listen to
        // nothing, so it goes away with the desktop. The user's preference
        // survives: pairing again starts the watch again.
        AgentWatchService.stop(getApplication<Application>())
        // Keep this VM's private draft scope while disconnected; a newly paired
        // desktop replaces it before any paired screen becomes visible.
        mutableState.value = RemoteState(restoring = false, draftNamespace = state.value.draftNamespace)
    }

    /**
     * Start the background watch when the user asked for it.
     *
     * The watch follows the credential: a restored or freshly paired desktop
     * gives it something to listen to, and the service stops itself again when
     * the desktop goes away.
     */
    private fun startWatchIfEnabled() {
        val app = getApplication<Application>()
        if (AgentWatchService.isEnabled(app)) AgentWatchService.start(app)
    }
    fun unauthorized() {
        forget()
        mutableState.update { it.copy(error = "This device is no longer authorized. Scan a fresh invitation from the desktop.") }
    }
    override fun onCleared() { actionJob?.cancel(); pause(); api?.close() }
}

private fun draftNamespace(session: DeviceSession): String {
    val origin = session.origin.toHttpUrl()
    val identity = "${origin.scheme}://${origin.host}:${origin.port}\u0000".toByteArray(Charsets.UTF_8) + (session.root ?: byteArrayOf())
    val digest = MessageDigest.getInstance("SHA-256").digest(identity)
    return "desktop:" + Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
}

fun friendlyError(e: Exception): String = when (e) {
    is ApiException, is IllegalArgumentException, is IllegalStateException -> e.message.orEmpty()
    is javax.net.ssl.SSLHandshakeException, is javax.net.ssl.SSLPeerUnverifiedException ->
        "Desktop certificate changed or could not be verified. Pair again using a fresh QR code."
    else -> "Cannot reach the desktop. Check that Buildmesh is running and your network or VPN is connected."
}
