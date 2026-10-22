package dev.buildmesh.remote

import android.app.Service
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import androidx.core.content.edit
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import java.util.concurrent.ConcurrentHashMap

/**
 * The phone's background watch for agents that need a person.
 *
 * The service holds the desktop's event feed open while the user has enabled
 * background notifications, so a node that asks a question or requests
 * permission reaches the phone even when no screen is in front of the user.
 * The notice is cleared as soon as the node's state is resolved — including
 * from the desktop, which is the whole point: the answer does not have to be
 * given on the phone that was told about it.
 *
 * The service is off until the user turns it on, and it stops as soon as it is
 * turned off.
 */
class AgentWatchService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val notices = ConcurrentHashMap<Long, String>()
    private val names = ConcurrentHashMap<Long, String>()

    /**
     * Whether this service is on its way out.
     *
     * The pairing read is asynchronous, so a service that is stopped while it
     * reads must not attach itself to the feed afterwards.
     */
    @Volatile private var stopped = false

    private val listener = object : FeedListener {
        override fun onOpen() { scope.launch { reconcile() } }
        override fun onEvent(event: FeedEvent) { scope.launch { handle(event) } }
        override fun onUnauthorized() { stopSelf() }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        AppEventFeed.bind(applicationContext)
        AgentNotifications.ensureChannels(this)
        ServiceCompat.startForeground(
            this,
            AgentNotifications.WATCH_NOTIFICATION_ID,
            AgentNotifications.watchNotification(this),
            foregroundServiceType(),
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (!isEnabled(this)) {
            stopSelf()
            return START_NOT_STICKY
        }
        scope.launch {
            // A watch without a paired desktop has nothing to listen to, so it
            // says so by stopping rather than by sitting on a dead socket.
            val paired = withContext(Dispatchers.IO) { SessionStore(applicationContext).read() != null }
            if (stopped) return@launch
            if (paired) AppEventFeed.feed.attach(listener) else stopSelf()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        stopped = true
        AppEventFeed.feed.detach(listener)
        // The watch's own notification goes with the watch. Without this it
        // would outlive a service that stopped itself — revoked, disabled, or
        // unpaired — and sit in the drawer claiming the phone is listening.
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        // A notice lives as long as the watch does and the node still needs a
        // person, so a watch that ends takes its notices with it.
        for (nodeId in notices.keys) AgentNotifications.cancel(this, nodeId)
        notices.clear()
        scope.cancel()
        super.onDestroy()
    }

    private fun foregroundServiceType(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
        } else {
            0
        }

    private suspend fun handle(event: FeedEvent) {
        when (event) {
            is FeedEvent.Lifecycle -> if (event.needsPerson) {
                announce(event.nodeId, event.reason, event.message)
            } else {
                clear(event.nodeId)
            }
            is FeedEvent.Cleared -> clear(event.nodeId)
            is FeedEvent.Other -> Unit
        }
    }

    /**
     * Bring the notices in line with the desktop's own inventory.
     *
     * The socket can open long after a node asked its question, so the
     * inventory is read once on every open: a node that is still waiting gets
     * its notice, and a node that was answered while the phone was away loses
     * the notice it never saw.
     */
    private suspend fun reconcile() {
        val nodes = readNodes() ?: return
        val live = mutableSetOf<Long>()
        for (node in nodes) {
            val attention = nodeAttention(node)
            if (attention.nodeId == 0L) continue
            names[attention.nodeId] = node.text("name")
            live.add(attention.nodeId)
            if (attention.needsPerson) {
                announce(attention.nodeId, attention.reason, attention.message)
            } else {
                clear(attention.nodeId)
            }
        }
        // A node that is gone from the inventory cannot still need a person.
        for (nodeId in notices.keys.toList()) if (nodeId !in live) clear(nodeId)
    }

    private suspend fun announce(nodeId: Long, reason: String, message: String) {
        val body = listOf(reason, message).filter { it.isNotBlank() }.joinToString(" · ")
        val previous = notices.put(nodeId, body)
        if (previous == body) return
        val title = nameOf(nodeId).ifEmpty { "Agent Node" }
        AgentNotifications.post(this, nodeId, title, body)
    }

    private fun clear(nodeId: Long) {
        notices.remove(nodeId)
        AgentNotifications.cancel(this, nodeId)
    }

    /**
     * The node's name, read from the desktop the first time it is needed.
     *
     * The event feed names a node by id only, so the title has to come from
     * the inventory. One read serves every node the phone has not seen yet.
     */
    private suspend fun nameOf(nodeId: Long): String {
        names[nodeId]?.let { return it }
        val nodes = readNodes() ?: return ""
        var name = ""
        for (node in nodes) {
            val id = node.number("id")
            if (id == 0L) continue
            names[id] = node.text("name")
            if (id == nodeId) name = node.text("name")
        }
        return name
    }

    private suspend fun readNodes(): List<JsonObject>? {
        val client = AppEventFeed.feed.client() ?: return null
        return try {
            client.get("/api/nodes").objects()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            null
        }
    }

    companion object {
        private const val PREFS = "settings"
        private const val KEY_BACKGROUND = "background-agent-notifications"

        /** Whether the user has asked the phone to watch for agents in the background. */
        fun isEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFS, 0).getBoolean(KEY_BACKGROUND, false)

        /** Turn the background watch on or off, and tell the service about it. */
        fun setEnabled(context: Context, enabled: Boolean) {
            context.getSharedPreferences(PREFS, 0).edit { putBoolean(KEY_BACKGROUND, enabled) }
            if (enabled) start(context) else stop(context)
        }

        /** Start watching for agents in the background. */
        fun start(context: Context) {
            ContextCompat.startForegroundService(context, serviceIntent(context))
        }

        /**
         * Stop watching, and take the notices down with it.
         *
         * The notices are cleared here as well as in the service, because a
         * service the system killed earlier is no longer there to clear them.
         */
        fun stop(context: Context) {
            context.stopService(serviceIntent(context))
            AgentNotifications.cancelAll(context)
        }

        /**
         * The intent that names this service.
         *
         * The component is set explicitly rather than through the
         * `Intent(Context, Class)` constructor, so the intent this service is
         * stopped with is recognisable as the same component it was started
         * with.
         */
        private fun serviceIntent(context: Context): Intent =
            Intent().setComponent(ComponentName(context, AgentWatchService::class.java))
    }
}
