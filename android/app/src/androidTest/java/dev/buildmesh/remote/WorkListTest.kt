package dev.buildmesh.remote

import android.graphics.Bitmap
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.toPixelMap
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.test.platform.app.InstrumentationRegistry
import dev.buildmesh.remote.wire.CircuitAgentOwnership
import kotlinx.serialization.json.Json
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class WorkListTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private val meshes = Json.parseToJsonElement("""[{"id":1,"name":"Buildmesh"},{"id":2,"name":"Website"}]""").objects()
    private val nodes = Json.parseToJsonElement("""[
        {"id":11,"mesh_id":1,"position":2,"name":"Implementer with a long task name that should truncate","status":"running","branch":"feat/work-sidebar-parity"},
        {"id":12,"mesh_id":1,"position":0,"name":"Reviewer","status":"awaiting_input","branch":"feat/work-sidebar-parity"},
        {"id":13,"mesh_id":1,"position":3,"name":"Standalone agent","status":"idle","branch":"fix/layout"},
        {"id":21,"mesh_id":2,"position":1,"name":"Website agent","status":"ready","branch":"main"}
    ]""").objects()
    private val state = RemoteState(meshes = meshes, nodes = nodes, circuitOwnerships = listOf(
        CircuitAgentOwnership(11, 7, 3, "Review", "running", null),
        CircuitAgentOwnership(12, 7, 3, "Review", "running", 11),
    ))

    @Test fun swipeRevealsCloseAndReverseSwipeOrTapHidesItWithoutOpening() {
        val opened = mutableListOf<Long>()
        val closed = mutableListOf<Long>()
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Box(Modifier.width(320.dp).fillMaxHeight()) {
                    Dashboard(state, "Work", 1, {}, { opened += it }, { _, _ -> }, { _, _ -> }, { closed += it })
                }
            }
        } }
        val reviewer = compose.onNodeWithText("Reviewer")
        reviewer.performScrollTo().performTouchInput { swipeLeft() }
        compose.onNodeWithText("Close", substring = false).assertIsDisplayed()
        saveScreenshot("work-close-revealed.png")
        compose.runOnIdle { assertTrue(opened.isEmpty()); assertTrue(closed.isEmpty()) }
        reviewer.performTouchInput { swipeRight() }
        compose.onNodeWithText("Close", substring = false).assertDoesNotExist()
        reviewer.performTouchInput { swipeLeft() }
        reviewer.performClick()
        compose.onNodeWithText("Close", substring = false).assertDoesNotExist()
        compose.runOnIdle { assertTrue(opened.isEmpty()); assertTrue(closed.isEmpty()) }
        reviewer.performClick()
        compose.runOnIdle { assertEquals(listOf(12L), opened) }
    }

    @Test fun closeConfirmsOnlyTheSwipedMemberAndCancelPreservesTheGroup() {
        val closed = mutableListOf<Long>()
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state, "Work", 1, {}, {}, { _, _ -> }, { _, _ -> }, { closed += it })
            }
        } }
        compose.onNodeWithText("Reviewer").performScrollTo().performTouchInput { swipeLeft() }
        compose.onNodeWithText("Close", substring = false).performClick()
        compose.onNodeWithText("Close Reviewer?").assertIsDisplayed()
        compose.onNodeWithText("Cancel").performClick()
        compose.runOnIdle { assertTrue(closed.isEmpty()) }
        compose.onNodeWithContentDescription("Paired group of 2 agents").assertExists()
        compose.onNodeWithText("Close", substring = false).performClick()
        compose.onNodeWithText("Close agent", substring = false).performClick()
        compose.runOnIdle { assertEquals(listOf(12L), closed) }
    }

    @Test fun rightToLeftSwipeRevealsTheTrailingActionAtLargeFontSize() {
        compose.setContent { BuildmeshTheme {
            val density = LocalDensity.current.density
            CompositionLocalProvider(LocalLayoutDirection provides LayoutDirection.Rtl,
                LocalDensity provides Density(density, 1.4f)) {
                Surface(color = MeshColors.background) {
                    Box(Modifier.width(320.dp).fillMaxHeight()) {
                        Dashboard(state, "Work", 1, {}, {}, { _, _ -> }, { _, _ -> }, {})
                    }
                }
            }
        } }
        compose.onNodeWithText("Reviewer").performScrollTo().performTouchInput { swipeRight() }
        compose.onNodeWithText("Close", substring = false).assertIsDisplayed().performClick()
        compose.onNodeWithText("Close Reviewer?").assertIsDisplayed()
        compose.onNodeWithText("Cancel").performClick()
        saveScreenshot("work-close-rtl.png")
    }

    @Test fun accessibilityCanRequestCloseAndBusyStateBlocksConfirmation() {
        val closed = mutableListOf<Long>()
        var busy by mutableStateOf(false)
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Dashboard(state.copy(busy = busy), "Work", 1, {}, {}, { _, _ -> }, { _, _ -> }, { closed += it })
            }
        } }
        val closeAction = compose.onNodeWithText("Reviewer").performScrollTo().fetchSemanticsNode()
            .config[SemanticsActions.CustomActions].single { it.label == "Close Reviewer" }
        compose.runOnIdle { assertTrue(closeAction.action()) }
        compose.runOnIdle { busy = true }
        compose.onNodeWithText("Close agent", substring = false).assertIsNotEnabled()
        compose.runOnIdle { assertTrue(closed.isEmpty()); busy = false }
        compose.onNodeWithText("Close agent", substring = false).performClick()
        compose.runOnIdle { assertEquals(listOf(12L), closed) }
    }

    @Test fun narrowWorkGroupsPairsAndEachRowOpensItsOwnAgent() {
        val opened = mutableListOf<Long>()
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Box(Modifier.width(320.dp).fillMaxHeight().testTag("work")) {
                    Dashboard(state, "Work", 1, {}, { opened += it }, { _, _ -> }, { _, _ -> })
                }
            }
        } }
        compose.onNodeWithContentDescription("Paired group of 2 agents").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Reviewer").assertIsDisplayed()
        compose.onNodeWithContentDescription("Running. Circuit active", useUnmergedTree = true).assertIsDisplayed()
        compose.onNodeWithContentDescription("Needs attention. Circuit waiting", useUnmergedTree = true).assertIsDisplayed()
        val implementer = compose.onNodeWithText("Implementer with a long task name that should truncate")
        val reviewer = compose.onNodeWithText("Reviewer")
        val firstBounds = implementer.fetchSemanticsNode().boundsInRoot
        val secondBounds = reviewer.fetchSemanticsNode().boundsInRoot
        assertEquals(firstBounds.left, secondBounds.left, 1f)
        assertTrue(firstBounds.bottom <= secondBounds.top)
        assertTrue(firstBounds.right <= compose.onNodeWithTag("work").fetchSemanticsNode().boundsInRoot.right)
        implementer.performClick()
        reviewer.performClick()
        compose.runOnIdle { assertEquals(listOf(11L, 12L), opened) }
        saveScreenshot("work-sidebar.png")
    }

    private fun saveScreenshot(name: String) {
        val bitmap = compose.onRoot().captureToImage().asAndroidBitmap()
        try {
            InstrumentationRegistry.getInstrumentation().targetContext.cacheDir.resolve(name).outputStream().use {
                assertTrue(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it))
            }
        } finally { bitmap.recycle() }
    }

    @Test fun aSelectedMeshKeepsItsPairAtLargeFontSizes() {
        compose.setContent { BuildmeshTheme {
            val density = LocalDensity.current.density
            CompositionLocalProvider(LocalDensity provides Density(density, 1.4f)) {
                Surface(color = MeshColors.background) {
                    Box(Modifier.width(320.dp).fillMaxHeight()) {
                        Dashboard(state, "Work", 1, {}, {}, { _, _ -> }, { _, _ -> })
                    }
                }
            }
        } }
        compose.onNode(hasText("Reviewer") and hasClickAction()).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Implementer with a long task name that should truncate").performScrollTo().assertIsDisplayed()
        compose.onNodeWithContentDescription("Paired group of 2 agents").assertExists()
        compose.onNodeWithText("Standalone agent").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Website agent").assertDoesNotExist()
        compose.onNodeWithText("Search agents or branches").assertDoesNotExist()
        compose.onNodeWithText("All meshes").assertDoesNotExist()
    }

    @Test fun runningIsFilledAndIdleIsHollowInTheNativeWindow() {
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) {
                Column {
                    Box(Modifier.testTag("running")) { WorkStatusGlyph(nodes[0], null) }
                    Box(Modifier.testTag("idle")) { WorkStatusGlyph(nodes[2], null) }
                }
            }
        } }
        val running = compose.onNodeWithTag("running").captureToImage().toPixelMap()
        val idle = compose.onNodeWithTag("idle").captureToImage().toPixelMap()
        assertEquals(MeshColors.accent.toArgb(), running[running.width / 2, running.height / 2].toArgb())
        assertEquals(MeshColors.background.toArgb(), idle[idle.width / 2, idle.height / 2].toArgb())
    }
}
