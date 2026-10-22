package dev.buildmesh.remote

import kotlinx.serialization.json.*

/**
 * The scheme a node link uses.
 *
 * It is the app's own scheme, so a link cannot be pointed at another app.
 */
const val NODE_LINK_SCHEME = "buildmesh"

/**
 * The node status that means an Agent Node is blocked on a person.
 *
 * The desktop keeps one source of truth for "this node needs a person": the
 * node's own status. The lifecycle kind only says *why*, so it picks the
 * wording and never the decision — the same split the desktop itself makes
 * when it derives `is_attention_pending` from `node.status`.
 */
const val NEEDS_PERSON_STATUS = "awaiting_input"

/**
 * The wording for a node that needs a person, per lifecycle kind. These are
 * the words the node list already uses, so a notification and the row behind
 * it say the same thing.
 */
private val PERSON_NEEDED_REASONS = mapOf(
    "question_requested" to "Needs an answer",
    "permission_requested" to "Needs permission",
)

/** Why a node needs a person, for the lifecycle kind it reported. */
fun attentionReason(kind: String): String = PERSON_NEEDED_REASONS[kind] ?: "Needs attention"

/**
 * The first id of the range that carries one notification per node. Node
 * notices live in their own range so clearing one node never touches the
 * watch service's own notification.
 */
private const val ATTENTION_NOTIFICATION_BASE = 20_000

/**
 * A stable notification id per node, so a node's notice is replaced rather
 * than stacked, and one node's notice never replaces another's.
 */
fun attentionNotificationId(nodeId: Long): Int = ATTENTION_NOTIFICATION_BASE + nodeId.toInt()

/**
 * The node a `buildmesh://node/<id>` link names.
 *
 * Returns 0 for any other link, so a pairing link never opens a node and a
 * malformed id never opens the wrong one.
 */
fun nodeIdFromLink(link: String): Long {
    val prefix = "$NODE_LINK_SCHEME://node/"
    if (!link.startsWith(prefix)) return 0L
    return link.removePrefix(prefix).substringBefore('/').substringBefore('?').toLongOrNull() ?: 0L
}

/**
 * One frame of the desktop's event feed.
 *
 * The feed carries more than node lifecycle — circuit runs, refused input —
 * so an unrecognised frame is still reported: the dashboard refreshes for
 * every frame, and only the node-scoped ones mean anything to the watch
 * service.
 */
sealed interface FeedEvent {
    /** The desktop reported a node's new lifecycle status. */
    data class Lifecycle(val nodeId: Long, val kind: String, val status: String, val message: String) : FeedEvent {
        /** This node is blocked on a person. */
        val needsPerson: Boolean get() = status == NEEDS_PERSON_STATUS

        /** Why this node is blocked on a person. */
        val reason: String get() = attentionReason(kind)
    }

    /** The desktop cleared a node's person-needed state. */
    data class Cleared(val nodeId: Long) : FeedEvent

    /** Any other feed frame. */
    data class Other(val type: String) : FeedEvent
}

/**
 * Read one `/ws/events` frame.
 *
 * A frame that is not JSON, or not an object, is reported as `Other` rather
 * than dropped: the dashboard still refreshes, and a malformed frame never
 * takes the feed down.
 */
fun parseFeedEvent(text: String): FeedEvent {
    val frame = runCatching { Json.parseToJsonElement(text).obj() }.getOrNull() ?: return FeedEvent.Other("")
    return when (frame.text("type")) {
        "agent-lifecycle" -> FeedEvent.Lifecycle(
            nodeId = frame.number("session_id"),
            kind = frame.text("kind"),
            status = frame.text("status"),
            message = frame.text("message"),
        )
        "attention-cleared" -> FeedEvent.Cleared(frame.number("session_id"))
        else -> FeedEvent.Other(frame.text("type"))
    }
}

/**
 * A node row's own person-needed state.
 *
 * The node row carries the same lifecycle payload the event carries, so the
 * catch-up read after a reconnect reaches the same conclusion the live event
 * did. The kind is only read while the lifecycle is the current revision —
 * the same rule the node list applies when it labels a node.
 */
fun nodeAttention(node: JsonObject): FeedEvent.Lifecycle {
    val lifecycle = node.objectAt("lifecycle")
    val status = node.text("status")
    val kind = lifecycle?.takeIf { it.text("status") == status }?.text("kind").orEmpty()
    return FeedEvent.Lifecycle(
        nodeId = node.number("id"),
        kind = kind,
        status = status,
        message = lifecycle?.text("message").orEmpty(),
    )
}
