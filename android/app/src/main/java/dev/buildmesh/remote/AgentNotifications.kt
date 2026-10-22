package dev.buildmesh.remote

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.net.toUri

/**
 * The phone's notification surface for agents that need a person.
 *
 * Two channels, because they answer different questions. "Agent needs you"
 * carries the request itself and is worth a sound. "Background agent watch"
 * is the watch service's own persistent notice: it says the phone is
 * listening, and it must stay quiet.
 */
object AgentNotifications {
    /** The channel an agent's request for a person arrives on. */
    const val ATTENTION_CHANNEL = "buildmesh.agent-attention"

    /** The channel the watch service's own persistent notification lives on. */
    const val WATCH_CHANNEL = "buildmesh.agent-watch"

    /** The id of the watch service's persistent notification. */
    const val WATCH_NOTIFICATION_ID = 1

    /** Create the channels this app posts on. */
    fun ensureChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val attention = NotificationChannel(
            ATTENTION_CHANNEL,
            "Agent needs you",
            NotificationManager.IMPORTANCE_HIGH,
        ).apply { description = "An agent asked a question or requested permission." }
        val watch = NotificationChannel(
            WATCH_CHANNEL,
            "Background agent watch",
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Keeps the phone connected to the desktop so agent requests can arrive."
            setShowBadge(false)
        }
        manager.createNotificationChannel(attention)
        manager.createNotificationChannel(watch)
    }

    /** Whether this phone is allowed to post notifications at all. */
    fun allowed(context: Context): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED

    /**
     * Post one agent's request for a person.
     *
     * The notice replaces that node's earlier one rather than stacking, so a
     * node that asks twice leaves one notice behind.
     */
    fun post(context: Context, nodeId: Long, title: String, body: String) {
        // The permission check is written out here rather than delegated to
        // `allowed()`: lint's permission analysis only recognises a guard in
        // the method that makes the call, and this module builds with
        // warnings as errors, so a delegated check fails the gate.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) return
        val notification = NotificationCompat.Builder(context, ATTENTION_CHANNEL)
            .setSmallIcon(R.drawable.relay_monochrome)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_REMINDER)
            .setAutoCancel(true)
            .setContentIntent(openNode(context, nodeId))
            .build()
        NotificationManagerCompat.from(context).notify(attentionNotificationId(nodeId), notification)
    }

    /** Clear one node's notice. */
    fun cancel(context: Context, nodeId: Long) {
        NotificationManagerCompat.from(context).cancel(attentionNotificationId(nodeId))
    }

    /** Clear every notice this app has posted, the watch service's own included. */
    fun cancelAll(context: Context) {
        NotificationManagerCompat.from(context).cancelAll()
    }

    /** The watch service's own notification: the phone is listening for agents. */
    fun watchNotification(context: Context): Notification =
        NotificationCompat.Builder(context, WATCH_CHANNEL)
            .setSmallIcon(R.drawable.relay_monochrome)
            .setContentTitle("Watching for agents")
            .setContentText("Buildmesh will notify you when an agent needs you.")
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setOngoing(true)
            .setSilent(true)
            .setContentIntent(openApp(context))
            .build()

    /** The intent that opens one Agent Node. */
    private fun openNode(context: Context, nodeId: Long): PendingIntent {
        val intent = Intent(
            Intent.ACTION_VIEW,
            "$NODE_LINK_SCHEME://node/$nodeId".toUri(),
            context,
            MainActivity::class.java,
        ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(
            context,
            nodeId.toInt(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /** The intent that opens the app itself. */
    private fun openApp(context: Context): PendingIntent {
        val intent = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(
            context,
            WATCH_NOTIFICATION_ID,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
