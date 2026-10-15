package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CornerBasedShape
import androidx.compose.foundation.shape.CornerSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonObject

/**
 * The phone's counterpart of the desktop spawn cluster: the main half launches
 * with the mesh default, the chevron opens the backend menu in its own order.
 * `subject` names what is being started ("issue #42") for screen readers.
 */
@Composable
fun SpawnSplitButton(
    defaultName: String?,
    options: List<JsonObject>,
    subject: String,
    enabled: Boolean,
    onDefault: () -> Unit,
    onChoose: (JsonObject) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val corners = MaterialTheme.shapes.small as CornerBasedShape
    val label = defaultName ?: "Mesh default"
    Row(horizontalArrangement = Arrangement.spacedBy(1.dp)) {
        Button(
            shape = corners.copy(topEnd = CornerSize(0.dp), bottomEnd = CornerSize(0.dp)),
            enabled = enabled, onClick = onDefault,
            contentPadding = PaddingValues(start = 12.dp, end = 14.dp),
            modifier = Modifier.weight(1f, fill = false).semantics { contentDescription = "Start $subject with $label" },
        ) {
            Icon(Icons.Default.Add, null, Modifier.size(18.dp))
            Spacer(Modifier.width(6.dp))
            Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Box {
            Button(
                shape = corners.copy(topStart = CornerSize(0.dp), bottomStart = CornerSize(0.dp)),
                enabled = enabled && options.isNotEmpty(), onClick = { expanded = true },
                contentPadding = PaddingValues(horizontal = 10.dp),
                modifier = Modifier.semantics { contentDescription = "Choose how to start $subject" },
            ) { Icon(Icons.Default.ExpandMore, null) }
            DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                options.forEach { option ->
                    val configuration = option.objectAt("configuration") != null
                    DropdownMenuItem(enabled = enabled, onClick = { expanded = false; onChoose(option) }, text = {
                        Column {
                            Text(option.text("label"))
                            if (configuration) Text("Saved configuration", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
                        }
                    })
                }
            }
        }
    }
}
