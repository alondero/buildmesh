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

data class RemoteState(
    val restoring: Boolean = true,
    val paired: Boolean = false,
    val busy: Boolean = false,
    val refreshing: Boolean = false,
    val error: String = "",
    val notice: String = "",
    val origin: String = "",
    val draftNamespace: String = "",
    val meshes: List<JsonObject> = emptyList(),
    val nodes: List<JsonObject> = emptyList(),
    val providers: List<JsonObject> = emptyList(),
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
    private var events: RemoteSocket? = null
    private var foreground = false
    private val refresher = RefreshController(viewModelScope,
        read = {
            val client = api ?: throw CancellationException("Disconnected")
            coroutineScope {
                val meshes = async { client.get("/api/meshes").objects() }
                val nodes = async { client.get("/api/nodes").objects() }
                val providers = async { client.get("/api/providers").objects() }
                Triple(meshes.await(), nodes.await(), providers.await())
            }
        }, publish = { snapshot ->
            mutableState.update { it.copy(meshes = snapshot.first, nodes = snapshot.second, providers = snapshot.third, error = "") }
        }, onError = ::report,
        onBusy = { busy -> mutableState.update { it.copy(refreshing = busy) } },
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
                } catch (e: Exception) {
                    if (e is ApiException && e.unauthorized) unauthorized()
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
                api?.close()
                api = BuildmeshApi(session.origin, session.root, session.cookie)
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
        val client = api ?: return
        if (state.value.restoring || polling?.isActive == true) return
        refresh()
        polling = viewModelScope.launch { while (isActive) { delay(5000); refresh() } }
        events = RemoteSocket(client, viewModelScope, "events", onOpen = { refresh() }, onData = {
            // Invalidate an in-flight snapshot before reading the new authoritative state.
            refresher.invalidate()
            refresh()
        }, onUnauthorized = ::unauthorized).also { it.start() }
    }

    fun pause() {
        foreground = false
        polling?.cancel(); polling = null
        refresher.stop()
        events?.stop(); events = null
        mutableState.update { it.copy(refreshing = false) }
    }

    fun refresh() {
        if (api != null && foreground && !state.value.restoring) refresher.refresh()
    }

    fun acceptNode(node: JsonObject) {
        refresher.invalidate()
        mutableState.update { it.copy(nodes = it.nodes.filterNot { existing -> existing.number("id") == node.number("id") } + node) }
        refresh()
    }

    fun retry() {
        mutableState.update { it.copy(resourceRevision = it.resourceRevision + 1) }
        refresh()
    }

    fun action(block: suspend (BuildmeshApi) -> Unit) {
        val client = api ?: return
        if (state.value.busy) return
        val owner = generation
        mutableState.update { it.copy(busy = true, error = "", notice = "") }
        actionJob = viewModelScope.launch(start = CoroutineStart.LAZY) {
            try {
                block(client)
                if (owner == generation) refresh()
            } catch (e: CancellationException) { throw e
            } catch (e: Exception) {
                if (owner == generation) {
                    if (e is ApiException && e.unauthorized) unauthorized()
                    else mutableState.update { it.copy(error = friendlyError(e)) }
                }
            } finally { if (owner == generation) {
                actionJob = null
                mutableState.update { it.copy(busy = false) }
            } }
        }
        actionJob?.start()
    }

    fun notice(message: String) { mutableState.update { it.copy(notice = message) } }
    fun report(e: Exception) {
        if (e is ApiException && e.unauthorized) unauthorized()
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
        // Keep this VM's private draft scope while disconnected; a newly paired
        // desktop replaces it before any paired screen becomes visible.
        mutableState.value = RemoteState(restoring = false, draftNamespace = state.value.draftNamespace)
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
