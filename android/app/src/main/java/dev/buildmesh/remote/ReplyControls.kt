package dev.buildmesh.remote

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/**
 * The keystrokes the desktop's approval pair types into the agent's prompt.
 *
 * The desktop writes `y\r` to allow and `n\r` to reject. The phone sends the
 * same bytes, because a different sequence would answer a different question.
 */
const val ALLOW_SEQUENCE = "y\r"
const val DENY_SEQUENCE = "n\r"

/**
 * The reply state for one input request.
 *
 * Both the waiting deck and the terminal's own band answer the same request
 * with the same words and the same keystrokes, so the rule that retires a
 * control lives here, once:
 *
 * - a reply is marked in flight *before* the transport is asked, so a failure
 *   that lands immediately cannot be overwritten by the marker;
 * - the control is retired only once the desktop accepted the reply;
 * - a send that failed hands the control back instead of leaving it dead.
 */
class InputReply(
    /** The action in flight (`allow` / `deny`), or empty. */
    val pending: String,
    /** The action the desktop accepted, or empty. */
    val accepted: String,
    /** Whether the controls must not be used right now. */
    val blocked: Boolean,
    /** Send one decision; the transport reports back through `answer`. */
    val reply: (sequence: String, action: String) -> Unit,
)

/**
 * Remember the reply state for one input request.
 *
 * The marker belongs to *this* request, not to the node, so it is keyed by the
 * request: a node that answers one prompt and asks another arrives with live
 * controls rather than the previous one's disabled pair.
 *
 * @param answer reports whether the request reached the transport and
 *   completes with `true` once the desktop accepted it, or `false` when the
 *   send failed.
 */
@Composable
fun rememberInputReply(
    request: NodeInputRequest?,
    nodeId: Long,
    busy: Boolean,
    answer: (nodeId: Long, sequence: String, onDone: (accepted: Boolean) -> Unit) -> Boolean,
): InputReply {
    var pending by remember(request?.key) { mutableStateOf("") }
    var accepted by remember(request?.key) { mutableStateOf("") }
    // Any request in flight anywhere blocks these: one reply per decision, and
    // `answer` refuses a second one anyway.
    val blocked = busy || pending.isNotEmpty() || accepted.isNotEmpty()
    return InputReply(
        pending = pending,
        accepted = accepted,
        blocked = blocked,
        reply = { sequence, action ->
            pending = action
            if (!answer(nodeId, sequence, { ok -> if (ok) accepted = action else pending = "" })) pending = ""
        },
    )
}

/** One hairline reply chip: the phone web client's `.deck-chip`, hue-bordered. */
@Composable
fun ReplyChip(
    label: String,
    content: Color,
    border: Color,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    OutlinedButton(
        onClick = onClick,
        enabled = enabled,
        shape = MaterialTheme.shapes.small,
        colors = ButtonDefaults.outlinedButtonColors(contentColor = content),
        border = BorderStroke(1.dp, border),
        contentPadding = PaddingValues(horizontal = 8.dp),
        modifier = modifier,
    ) {
        Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}
