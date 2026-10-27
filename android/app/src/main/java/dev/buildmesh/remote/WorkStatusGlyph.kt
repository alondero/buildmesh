package dev.buildmesh.remote

import androidx.compose.animation.core.*
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.buildmesh.remote.wire.CircuitAgentOwnership
import kotlinx.serialization.json.JsonObject

class WorkGlyphMotion(val alpha: State<Float>, val angle: State<Float>)

private val StillGlyphMotion = WorkGlyphMotion(mutableFloatStateOf(1f), mutableFloatStateOf(0f))

@Composable
fun rememberWorkGlyphMotion(): WorkGlyphMotion {
    val animation = rememberInfiniteTransition(label = "Work glyphs")
    val alpha = animation.animateFloat(1f, 0.4f, infiniteRepeatable(tween(800), RepeatMode.Reverse), label = "Status pulse")
    val angle = animation.animateFloat(0f, 360f, infiniteRepeatable(tween(3200, easing = LinearEasing)), label = "Circuit orbit")
    return remember(alpha, angle) { WorkGlyphMotion(alpha, angle) }
}

// The shape comes from the generated status table — STATUS_CONFIG's `glyph`,
// drawn on NodeStatusGlyph's 24-unit grid — so a status this build does not
// know draws the explicit `Unknown` cross rather than the idle ring a
// hand-written `else ->` branch used to answer.
internal fun workStatusShape(status: String): String = MeshStatusTable.resolve(status).glyph

// The orbit says which animation a node gets, not what the statuses are
// called or how they look, so it names the statuses it reacts to directly
// rather than reading the generated vocabulary. A status outside these rules
// keeps the default orbit.
internal fun workCircuitOrbit(node: JsonObject, owner: CircuitAgentOwnership?): String? {
    if (owner == null || owner.state == "cancelled" || node.text("status") == "archived") return null
    return when {
        owner.state == "completed" -> "closed"
        owner.state !in setOf("pending", "running", "paused") -> "dashed"
        owner.state == "running" && node.text("status") in setOf("running", "spawning") -> "comet"
        node.text("status") in setOf("error", "lost") -> "dashed"
        else -> "half"
    }
}

@Composable
fun WorkStatusGlyph(node: JsonObject, owner: CircuitAgentOwnership?, motion: WorkGlyphMotion = StillGlyphMotion) {
    val status = node.text("status")
    val shape = workStatusShape(status)
    val orbit = workCircuitOrbit(node, owner)
    val circuitLabel = when (orbit) {
        "comet" -> "Circuit active"
        "half" -> "Circuit waiting"
        "dashed" -> "Circuit needs attention"
        "closed" -> "Circuit done"
        else -> null
    }
    val orbitColor = when (orbit) {
        "closed" -> MeshColors.green
        "half" -> MeshColors.amber
        "dashed" -> MeshColors.red
        else -> MeshColors.violet
    }
    Canvas(Modifier.size(24.dp).semantics {
        contentDescription = listOfNotNull(nodeLabel(node), circuitLabel).joinToString(". ")
    }) {
        val color = MeshColors.status(status).copy(alpha = if (status in MeshStatusTable.pulsing) motion.alpha.value else 1f)
        scale(size.width / 24f, size.height / 24f, pivot = Offset.Zero) {
            val center = Offset(12f, 12f)
            fun ring(radius: Float, width: Float, dashed: Boolean = false) = drawCircle(color, radius, center,
                style = Stroke(width, pathEffect = if (dashed) PathEffect.dashPathEffect(floatArrayOf(2.2f, 1.9f)) else null))
            if (orbit != null) {
                val box = Offset(3.6f, 3.6f)
                val bounds = Size(16.8f, 16.8f)
                when (orbit) {
                    "closed" -> drawCircle(orbitColor, 8.4f, center, style = Stroke(1.6f))
                    "dashed" -> drawCircle(orbitColor, 8.4f, center, style = Stroke(1.6f,
                        pathEffect = PathEffect.dashPathEffect(floatArrayOf(2.64f, 1.76f))))
                    "half" -> {
                        drawCircle(orbitColor.copy(alpha = 0.25f), 8.4f, center, style = Stroke(1f))
                        drawArc(orbitColor, 180f, 180f, false, box, bounds, style = Stroke(1.7f, cap = StrokeCap.Round))
                    }
                    "comet" -> {
                        drawCircle(orbitColor.copy(alpha = 0.25f), 8.4f, center, style = Stroke(1f))
                        rotate(motion.angle.value - 90f, center) {
                            drawArc(orbitColor.copy(alpha = 0.22f), -120f, 120f, false, box, bounds, style = Stroke(1.3f, cap = StrokeCap.Round))
                            drawArc(orbitColor.copy(alpha = 0.5f), -70f, 70f, false, box, bounds, style = Stroke(1.5f, cap = StrokeCap.Round))
                            drawArc(orbitColor, -26f, 26f, false, box, bounds, style = Stroke(1.7f, cap = StrokeCap.Round))
                            drawPath(Path().apply {
                                moveTo(20.4f, 9.3f); lineTo(21.2f, 11.2f); lineTo(23.1f, 12f); lineTo(21.2f, 12.8f)
                                lineTo(20.4f, 14.7f); lineTo(19.6f, 12.8f); lineTo(17.7f, 12f); lineTo(19.6f, 11.2f); close()
                            }, orbitColor)
                        }
                    }
                }
            }
            when (shape) {
                "solid" -> drawCircle(color, 4.5f, center)
                "ring" -> ring(3.7f, 1.6f)
                "thin" -> ring(3.7f, 1.2f)
                "dashed" -> ring(3.7f, 1.4f, true)
                "target" -> { ring(3.9f, 1.4f); drawCircle(color, 1.8f, center) }
                "half" -> {
                    ring(3.7f, 1.4f)
                    drawArc(color, 90f, 180f, true, Offset(8.3f, 8.3f), Size(7.4f, 7.4f))
                }
                "slash" -> { ring(3.7f, 1.6f); drawLine(color, Offset(9.2f, 14.8f), Offset(14.8f, 9.2f), 1.5f, StrokeCap.Round) }
                "cross" -> {
                    drawLine(color, Offset(9.2f, 9.2f), Offset(14.8f, 14.8f), 1.5f, StrokeCap.Round)
                    drawLine(color, Offset(14.8f, 9.2f), Offset(9.2f, 14.8f), 1.5f, StrokeCap.Round)
                }
            }
        }
    }
}
