package dev.buildmesh.remote

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.darkColorScheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

// Mirrors the dark tokens in src/App.css and DESIGN.md.
object MeshColors {
    val background = Color(0xff0a0a0e)
    val surface = Color(0xff111116)
    val overlay = Color(0xff15151c)
    val card = Color(0xff16161d)
    val cardHover = Color(0xff1b1b23)
    val input = Color(0xff0e0e13)
    val selection = Color(0xff1a2a3a)
    val borderStrong = Color.White.copy(alpha = 0.14f)
    val borderDefault = Color.White.copy(alpha = 0.07f)
    val borderSubtle = Color.White.copy(alpha = 0.05f)
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

@OptIn(ExperimentalTextApi::class)
val Geist = FontFamily(
    listOf(FontWeight.Normal, FontWeight.Medium, FontWeight.SemiBold, FontWeight.Bold, FontWeight.ExtraBold).map { weight ->
        Font(R.font.geist, weight = weight, variationSettings = FontVariation.Settings(FontVariation.weight(weight.weight)))
    },
)

private fun textStyle(size: Int, lineHeight: Int, weight: FontWeight = FontWeight.Normal) =
    TextStyle(fontFamily = Geist, fontWeight = weight, fontSize = size.sp, lineHeight = lineHeight.sp)

private val BuildmeshTypography = Typography(
    displayLarge = textStyle(40, 48, FontWeight.SemiBold),
    displayMedium = textStyle(32, 40, FontWeight.SemiBold),
    displaySmall = textStyle(28, 36, FontWeight.SemiBold),
    headlineLarge = textStyle(28, 36, FontWeight.SemiBold),
    headlineMedium = textStyle(24, 32, FontWeight.SemiBold),
    headlineSmall = textStyle(22, 28, FontWeight.SemiBold),
    titleLarge = textStyle(20, 28, FontWeight.SemiBold),
    titleMedium = textStyle(16, 24, FontWeight.SemiBold),
    titleSmall = textStyle(14, 20, FontWeight.Medium),
    bodyLarge = textStyle(16, 24),
    bodyMedium = textStyle(14, 20),
    bodySmall = textStyle(12, 18),
    labelLarge = textStyle(14, 20, FontWeight.Medium),
    labelMedium = textStyle(12, 16, FontWeight.Medium),
    labelSmall = textStyle(11, 16, FontWeight.Medium),
)

@Composable
fun buildmeshTextFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedContainerColor = MeshColors.input, unfocusedContainerColor = MeshColors.input,
    disabledContainerColor = MeshColors.input, errorContainerColor = MeshColors.input,
    unfocusedBorderColor = MeshColors.borderDefault,
)

@Composable
fun BuildmeshTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = darkColorScheme(
        primary = MeshColors.accent, onPrimary = MeshColors.background,
        primaryContainer = MeshColors.selection, onPrimaryContainer = MeshColors.accent,
        inversePrimary = MeshColors.background,
        secondary = MeshColors.accent, onSecondary = MeshColors.background,
        secondaryContainer = MeshColors.selection, onSecondaryContainer = MeshColors.accent,
        tertiary = MeshColors.violet, onTertiary = MeshColors.text,
        tertiaryContainer = MeshColors.card, onTertiaryContainer = MeshColors.text,
        background = MeshColors.background,
        surface = MeshColors.surface, surfaceVariant = MeshColors.card,
        onBackground = MeshColors.text, onSurface = MeshColors.text,
        onSurfaceVariant = MeshColors.secondary,
        surfaceTint = Color.Transparent,
        inverseSurface = MeshColors.text, inverseOnSurface = MeshColors.background,
        surfaceDim = MeshColors.background, surfaceBright = MeshColors.cardHover,
        surfaceContainerLowest = MeshColors.background, surfaceContainerLow = MeshColors.surface,
        surfaceContainer = MeshColors.surface, surfaceContainerHigh = MeshColors.overlay,
        surfaceContainerHighest = MeshColors.card,
        outline = MeshColors.borderStrong, outlineVariant = MeshColors.borderSubtle,
        error = MeshColors.red, onError = MeshColors.background,
        errorContainer = MeshColors.red.copy(alpha = 0.10f), onErrorContainer = MeshColors.red,
        scrim = Color.Black,
    ), typography = BuildmeshTypography, shapes = Shapes(
        extraSmall = RoundedCornerShape(10.dp),
        small = RoundedCornerShape(8.dp),
        medium = RoundedCornerShape(10.dp),
        large = RoundedCornerShape(12.dp),
        extraLarge = RoundedCornerShape(12.dp),
    ), content = content)
}
