// Generated from src/lib/status.ts by scripts/generate-android-status-table.mjs.
// Do not edit: run `npm run android:status-table` and commit the result.
package dev.buildmesh.remote

/**
 * One row of the agent-status vocabulary the desktop and the mobile SPA share
 * (`STATUS_CONFIG` in `src/lib/status.ts`).
 *
 * @property hex the ARGB value Compose's `Color(Long)` reads.
 * @property glyph the desktop's circle shape; the phone draws the same shapes.
 * @property dot the text glyph the mobile SPA draws inline.
 * @property pulsing whether the status pulses, as STATUS_CONFIG's
 *   `animate-pulse-fast` class marks it.
 */
data class MeshStatus(
    val status: String,
    val label: String,
    val hex: Long,
    val glyph: String,
    val dot: String,
    val pulsing: Boolean,
)

/**
 * The statuses this build knows.
 *
 * A status the desktop reports that is missing here is an explicit gap rather
 * than a muted dot: [resolve] answers the [Unknown] marker, so the phone says
 * the status is unknown instead of showing a healthy-looking grey circle.
 */
object MeshStatusTable {
    val entries: List<MeshStatus> = listOf(
        MeshStatus("pending", "Starting…", 0xff7a8492L, "dashed", "◌", true),
        MeshStatus("spawning", "Starting…", 0xff7a8492L, "dashed", "◌", true),
        MeshStatus("running", "Running", 0xff00d4ffL, "solid", "●", false),
        MeshStatus("idle", "Idle", 0xff00d4ffL, "ring", "○", false),
        MeshStatus("awaiting_input", "Needs attention", 0xfff59e0bL, "target", "●", true),
        MeshStatus("error", "Error", 0xffef4444L, "cross", "✗", false),
        MeshStatus("lost", "Lost", 0xffef4444L, "slash", "⊘", false),
        MeshStatus("suspended", "Suspended", 0xff8b5cf6L, "half", "⏸", false),
        MeshStatus("completed", "PR opened", 0xff22c55eL, "target", "✓", false),
        MeshStatus("ready", "Ready", 0xff22c55eL, "target", "✓", false),
        MeshStatus("archived", "Archived", 0xff7a8492L, "thin", "◌", false),
    )

    /** The marker for a status this build does not know. */
    val Unknown = MeshStatus("unknown", "Unknown status", 0xffef4444L, "cross", "?", false)

    private val byStatus: Map<String, MeshStatus> = entries.associateBy { it.status }

    /** The statuses that pulse, as STATUS_CONFIG marks them. */
    val pulsing: Set<String> = setOf("pending", "spawning", "awaiting_input")

    /** The row for [status], or null when this build does not know that status. */
    fun find(status: String): MeshStatus? = byStatus[status]

    /** The row for [status], or the [Unknown] marker when this build does not know it. */
    fun resolve(status: String): MeshStatus = byStatus[status] ?: Unknown
}
