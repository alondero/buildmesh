package dev.buildmesh.remote

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

// Mirrors src/mobile/styles.css and DESIGN.md; all native screens consume these tokens.
object MeshColors {
    val background = Color(0xff0a0a0e)
    val surface = Color(0xff111116)
    val card = Color(0xff16161d)
    val text = Color(0xffe2e8f0)
    val secondary = Color(0xff94a3b8)
    val muted = Color(0xff7a8492)
    val accent = Color(0xff00d4ff)
    val violet = Color(0xff8b5cf6)
    val green = Color(0xff22c55e)
    val amber = Color(0xfff59e0b)
    val red = Color(0xffef4444)
    fun status(status: String) = when (status) {
        "awaiting_input" -> amber
        "ready", "completed" -> green
        "error", "lost" -> red
        "suspended" -> violet
        "running", "idle" -> accent
        else -> muted
    }
}

@Composable
fun BuildmeshTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = darkColorScheme(
        primary = MeshColors.accent, onPrimary = MeshColors.background,
        secondary = MeshColors.violet, background = MeshColors.background,
        surface = MeshColors.surface, surfaceVariant = MeshColors.card,
        onBackground = MeshColors.text, onSurface = MeshColors.text,
        onSurfaceVariant = MeshColors.secondary, error = MeshColors.red,
    ), content = content)
}
