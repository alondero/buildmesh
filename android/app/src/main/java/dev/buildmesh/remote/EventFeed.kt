package dev.buildmesh.remote

import android.content.Context
import kotlinx.coroutines.*
import java.util.concurrent.CopyOnWriteArrayList

/** One owner of the desktop's event feed. */
interface FeedListener {
    /** The feed's socket is open. */
    fun onOpen() {}

    /** One frame arrived. */
    fun onEvent(event: FeedEvent) {}

    /** The desktop refused this device's credential. */
    fun onUnauthorized() {}
}

/**
 * One owner of the desktop's `/ws/events` feed for the whole process.
 *
 * The dashboard and the background watch service need the same frames, and two
 * sockets would double the desktop's subscriber count, the phone's idle
 * traffic and the ticket mint rate. So the feed keeps exactly one socket and
 * one reconnect timer and hands the same frames to every owner.
 *
 * The feed builds its own desktop client from the stored device session, so it
 * keeps working when no Activity is alive — which is the whole point of the
 * watch service.
 */
class EventFeed(
    private val scope: CoroutineScope,
    private val session: suspend () -> DeviceSession?,
) {
    private val listeners = CopyOnWriteArrayList<FeedListener>()
    private val lock = Any()
    @Volatile private var client: BuildmeshApi? = null
    @Volatile private var socket: RemoteSocket? = null
    @Volatile private var opening: Job? = null
    @Volatile private var revoked = false
    private var generation = 0

    /** Start feeding this listener. */
    fun attach(listener: FeedListener) {
        if (!listeners.contains(listener)) listeners.add(listener)
        start()
    }

    /** Stop feeding this listener; the socket closes with the last one. */
    fun detach(listener: FeedListener) {
        listeners.remove(listener)
        if (listeners.isEmpty()) stop()
    }

    /**
     * Rebuild the feed against the stored credential.
     *
     * Pairing and forgetting both change the credential the desktop accepts,
     * so the socket has to be re-minted against the new one.
     */
    fun reload() {
        revoked = false
        if (listeners.isEmpty()) return
        stop()
        start()
    }

    /** The desktop client the feed is connected with, for owners that read beside it. */
    fun client(): BuildmeshApi? = client

    private fun start() {
        if (socket != null || opening?.isActive == true || revoked) return
        val owner = synchronized(lock) {
            if (socket != null || opening?.isActive == true || revoked) return
            ++generation
        }
        opening = scope.launch {
            val saved = try {
                session()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                null
            }
            if (saved == null) return@launch
            val api = BuildmeshApi(saved.origin, saved.root, saved.cookie)
            val events = RemoteSocket(
                api,
                scope,
                "events",
                onOpen = { listeners.forEach { it.onOpen() } },
                onData = { bytes -> deliver(bytes) },
                onUnauthorized = { refuse(api) },
            )
            // The socket is published under the lock, so a stop that lands
            // while the session is being read cannot leave a live socket
            // behind with nothing able to close it.
            synchronized(lock) {
                if (owner != generation) {
                    api.close()
                    return@launch
                }
                client = api
                socket = events
                events.start()
            }
        }
    }

    private fun stop() {
        synchronized(lock) {
            generation++
            opening?.cancel()
            opening = null
            socket?.stop()
            socket = null
            client?.close()
            client = null
        }
    }

    /**
     * The desktop refused this device's credential.
     *
     * The feed stops here instead of reconnecting: a revoked device would
     * otherwise mint a ticket, be refused, and mint another, forever. Pairing
     * again calls [reload], which is the only way back.
     */
    private fun refuse(api: BuildmeshApi) {
        synchronized(lock) {
            // A socket that has already been replaced has lost its claim on
            // the feed; its refusal must not revoke the socket that took
            // its place.
            if (client !== api) return
            revoked = true
            generation++
            socket = null
            client = null
        }
        api.close()
        listeners.forEach { it.onUnauthorized() }
    }

    private fun deliver(bytes: ByteArray) {
        val event = parseFeedEvent(bytes.toString(Charsets.UTF_8))
        listeners.forEach { it.onEvent(event) }
    }
}

/**
 * The app's single event feed.
 *
 * The dashboard and the watch service attach to the same feed, so the phone
 * holds one socket to the desktop however many surfaces are watching.
 */
object AppEventFeed {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    @Volatile private var store: SessionStore? = null

    val feed = EventFeed(scope) { withContext(Dispatchers.IO) { store?.read() } }

    /** Bind the feed to this device's stored session. Safe to call more than once. */
    fun bind(context: Context) {
        synchronized(this) {
            if (store == null) store = SessionStore(context.applicationContext)
        }
    }
}
