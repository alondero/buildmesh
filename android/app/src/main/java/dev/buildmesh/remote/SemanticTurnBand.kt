package dev.buildmesh.remote

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/**
 * The desktop's semantic-turn banner, on the phone.
 *
 * The desktop draws the pending request directly above the terminal, where the
 * request is read, with the approval pair the harness asked for. This is that
 * band: the request's own words, and the same pair, sending the same
 * keystrokes.
 *
 * A request Buildmesh cannot answer with a keystroke — a question, or one the
 * harness did not classify — shows its text and its enumerated answers as
 * guidance and no pair: a tap on a choice cannot be verified to select that
 * option in the agent's own prompt.
 */
@Composable
fun SemanticTurnBand(request: NodeInputRequest?, reply: InputReply, modifier: Modifier = Modifier) {
    if (request == null) return
    Column(modifier.fillMaxWidth()) {
        Row(
            Modifier
                .fillMaxWidth()
                .background(MeshColors.amber.copy(alpha = 0.15f))
                .padding(horizontal = 12.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    request.message.ifEmpty { "Waiting for the agent's prompt…" },
                    style = MaterialTheme.typography.bodySmall,
                    color = MeshColors.text,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
                // The answers the harness enumerated, as text: a tap here cannot
                // be verified to select the option in the agent's own prompt.
                request.choices.forEach { choice ->
                    Text(
                        "• $choice",
                        style = MaterialTheme.typography.bodySmall,
                        color = MeshColors.secondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (request.isPermission) {
                ReplyChip(
                    label = when {
                        reply.accepted == "allow" -> request.approvedLabel
                        reply.pending == "allow" -> "Sending…"
                        else -> request.approveLabel
                    },
                    content = MeshColors.green, border = MeshColors.approveBorder,
                    enabled = !reply.blocked,
                ) { reply.reply(ALLOW_SEQUENCE, "allow") }
                ReplyChip(
                    label = when {
                        reply.accepted == "deny" -> request.rejectedLabel
                        reply.pending == "deny" -> "Sending…"
                        else -> request.rejectLabel
                    },
                    content = MeshColors.red, border = MeshColors.rejectBorder,
                    enabled = !reply.blocked,
                ) { reply.reply(DENY_SEQUENCE, "deny") }
            }
        }
        HorizontalDivider(color = MeshColors.amber.copy(alpha = 0.35f), thickness = 1.dp)
    }
}
