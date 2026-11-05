package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.clickable
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.filled.Check
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.heading
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
    compact: Boolean = false,
) {
    var expanded by remember { mutableStateOf(false) }
    val corners = MaterialTheme.shapes.small as CornerBasedShape
    val label = defaultName ?: "Mesh default"
    Row(horizontalArrangement = Arrangement.spacedBy(1.dp)) {
        Button(
            shape = corners.copy(topEnd = CornerSize(0.dp), bottomEnd = CornerSize(0.dp)),
            enabled = enabled, onClick = onDefault,
            contentPadding = PaddingValues(start = 12.dp, end = 14.dp),
            modifier = (if (compact) Modifier.size(48.dp) else Modifier.weight(1f, fill = false))
                .semantics { contentDescription = "Start $subject with $label" },
        ) {
            Icon(Icons.Default.Add, null, Modifier.size(18.dp))
            if (!compact) {
                Spacer(Modifier.width(6.dp))
                Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        Box {
            Button(
                shape = corners.copy(topStart = CornerSize(0.dp), bottomStart = CornerSize(0.dp)),
                enabled = enabled && options.isNotEmpty(), onClick = { expanded = true },
                contentPadding = PaddingValues(horizontal = 10.dp),
                modifier = (if (compact) Modifier.size(48.dp) else Modifier)
                    .semantics { contentDescription = "Choose how to start $subject" },
            ) { Icon(Icons.Default.ExpandMore, null) }
            if (expanded) LaunchMenuSheet(
                label = "Choose how to start $subject", options = options, enabled = enabled,
                onDismiss = { expanded = false }, onChoose = { expanded = false; onChoose(it) },
            )
        }
    }
}

/** A single Material sheet replaces cascading menus on a touch screen. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LaunchMenuSheet(label: String, options: List<JsonObject>, enabled: Boolean,
    onDismiss: () -> Unit, onChoose: (JsonObject) -> Unit, selectedId: String? = null,
    defaultLabel: String? = null, onDefault: (() -> Unit)? = null,
) {
    val canDismiss by rememberUpdatedState(enabled)
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true,
        confirmValueChange = { canDismiss })
    ModalBottomSheet(onDismissRequest = { if (enabled) onDismiss() }, sheetState = sheetState,
        properties = ModalBottomSheetProperties(shouldDismissOnBackPress = enabled)) {
        Text(label, style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(horizontal = 24.dp))
        TextButton(enabled = enabled, onClick = onDismiss, modifier = Modifier.padding(horizontal = 12.dp)) { Text("Cancel") }
        LazyColumn(Modifier.fillMaxWidth().weight(1f, fill = false)) {
            if (defaultLabel != null && onDefault != null) item {
                ListItem(headlineContent = { Text(defaultLabel) },
                    trailingContent = { if (selectedId == "") Icon(Icons.Default.Check, null) },
                    modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp).clickable(enabled = enabled, onClick = onDefault)
                        .semantics { selected = selectedId == "" })
            }
            val groups = options.groupBy { it.text("harness_id") to it.text("login_id") }
            groups.forEach { (group, rows) ->
                item(key = "heading:$group") {
                    val native = options.firstOrNull { it.text("harness_id") == group.first && it.objectAt("configuration") == null }
                    val name = native?.text("label")?.substringBefore(" · ") ?: group.first
                    Text(listOf(name, rows.first().text("login_label")).filter { it.isNotEmpty() }.joinToString(" · "),
                        color = MeshColors.secondary, style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(horizontal = 24.dp, vertical = 12.dp).semantics { heading() })
                }
                items(rows, key = { it.text("id") }) { option ->
                    val reason = option.text("unavailable_reason")
                    val supporting = reason.ifEmpty { option.text("verification_warning") }
                        .ifEmpty { if (option.objectAt("configuration") != null) "Saved configuration" else "" }
                    ListItem(headlineContent = { Text(option.text("label")) },
                        supportingContent = if (supporting.isEmpty()) null else ({ Text(supporting) }),
                        trailingContent = { if (selectedId == option.text("id")) Icon(Icons.Default.Check, null) },
                        modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp)
                            .clickable(enabled = enabled && reason.isEmpty()) { onChoose(option) }
                            .semantics { selected = selectedId == option.text("id") })
                }
            }
        }
    }
}

@Composable
fun LaunchPicker(label: String, options: List<JsonObject>, value: String, onSelect: (String) -> Unit,
    enabled: Boolean, defaultLabel: String = "Mesh default") {
    var open by remember { mutableStateOf(false) }
    val selection = options.firstOrNull { it.text("id") == value }
    val selectedLabel = if (value.isEmpty()) defaultLabel else selection?.text("label") ?: "Previous choice unavailable"
    OutlinedButton(enabled = enabled, onClick = { open = true }, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)
        .semantics { contentDescription = "$label: $selectedLabel" }) {
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.labelMedium)
            Text(selectedLabel)
        }
        Icon(Icons.Default.ExpandMore, null)
    }
    if (open) LaunchMenuSheet(label, options, enabled, { open = false },
        { onSelect(it.text("id")); open = false }, selectedId = value,
        defaultLabel = defaultLabel, onDefault = { onSelect(""); open = false })
}
