package dev.buildmesh.remote

import androidx.activity.ComponentActivity
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.unit.dp
import androidx.compose.ui.graphics.toArgb
import androidx.test.espresso.Espresso.pressBack
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class LaunchSelectionTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private fun options(): List<JsonObject> {
        val native = JsonObject(MobileWireFixtures.array("providers").map { it.obj() }
            .single { it.text("id") == "codex" }.filterKeys { it != "unavailable_reason" } +
            mapOf("label" to JsonPrimitive("Codex")))
        return launchOptions(listOf(native), needsPrompt = true)
    }

    @Test fun materialSheetSelectsAccountWithTouchTargetsAndKeepsUnsignedRowsDisabled() {
        var selected: JsonObject? = null
        compose.setContent { BuildmeshTheme {
            LaunchMenuSheet("Choose a launch", options(), true, {}, { selected = it })
        } }
        compose.onNode(hasText("Codex · Guest") and hasClickAction()).performScrollTo().assertIsNotEnabled()
        val instrumentation = androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
        compose.waitUntil(5_000) {
            val frame = instrumentation.uiAutomation.takeScreenshot() ?: return@waitUntil false
            try { frame.getPixel(1, frame.height - 200) == MeshColors.surface.toArgb() }
            finally { frame.recycle() }
        }
        val screenshot = instrumentation.uiAutomation.takeScreenshot() ?: error("No native screenshot")
        try {
            instrumentation.targetContext.cacheDir.resolve("launch-selection.png").outputStream().use {
                assertTrue(screenshot.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it))
            }
        } finally { screenshot.recycle() }
        compose.onNode(hasText("Codex · Work") and hasClickAction()).performScrollTo().assertHeightIsAtLeast(48.dp).performClick()
        compose.runOnIdle {
            assertEquals("codex", createNodeBody(3, selected, "Fix it", "op-1").text("provider"))
            assertEquals("work", selected?.text("login_id"))
        }
    }

    @Test fun backCancelsWithoutChangingTheSelectionAndBusySheetOwnsDismissal() {
        var enabled by mutableStateOf(true)
        var open by mutableStateOf(true)
        var chosen = false
        compose.setContent { BuildmeshTheme {
            if (open) LaunchMenuSheet("Choose a launch", options(), enabled, { open = false }, { chosen = true })
        } }
        compose.onNodeWithText("Choose a launch").assertIsDisplayed()
        compose.runOnIdle { enabled = false }
        compose.onNodeWithText("Cancel").assertIsNotEnabled()
        pressBack()
        compose.onNodeWithText("Choose a launch").assertIsDisplayed()
        compose.runOnIdle { enabled = true }
        pressBack()
        compose.waitUntil(5_000) { !open }
        assertFalse(chosen)
    }
}
