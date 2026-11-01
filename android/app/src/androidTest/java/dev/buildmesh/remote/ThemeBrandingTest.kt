package dev.buildmesh.remote

import androidx.activity.ComponentActivity
import android.app.Application
import android.graphics.Bitmap
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.toPixelMap
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ThemeBrandingTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private fun saveScreenshot(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        compose.waitUntil(5_000) {
            instrumentation.uiAutomation.rootInActiveWindow?.packageName?.toString() == instrumentation.targetContext.packageName
        }
        assertEquals("Screenshot must show the native app", instrumentation.targetContext.packageName,
            instrumentation.uiAutomation.rootInActiveWindow?.packageName?.toString())
        val screenshot = instrumentation.uiAutomation.takeScreenshot() ?: error("No native screenshot")
        try {
            instrumentation.targetContext.cacheDir.resolve("$name.png").outputStream().use {
                assertTrue(screenshot.compress(Bitmap.CompressFormat.PNG, 100, it))
            }
        } finally { screenshot.recycle() }
    }

    @Test fun materialContainersUseBuildmeshColours() {
        lateinit var palette: ColorScheme
        compose.setContent {
            BuildmeshTheme {
                palette = MaterialTheme.colorScheme
                Column {
                    Text("Theme")
                    NavigationBar(Modifier.testTag("navigation")) { Text("Overview") }
                }
            }
        }
        compose.runOnIdle {
            assertEquals(0xff1a2a3a.toInt(), palette.secondaryContainer.toArgb())
            assertEquals(0xff00d4ff.toInt(), palette.onSecondaryContainer.toArgb())
            assertEquals(0xff111116.toInt(), palette.surfaceContainer.toArgb())
            assertEquals(0xff15151c.toInt(), palette.surfaceContainerHigh.toArgb())
            assertEquals(0xffef4444.toInt(), palette.onErrorContainer.toArgb())
        }
        val pixels = compose.onNodeWithTag("navigation").captureToImage().toPixelMap()
        assertEquals(0xff111116.toInt(), pixels[1, 1].toArgb())
    }

    @Test fun pairingRendersTheRelayBadgeAndWordmark() {
        compose.setContent { BuildmeshTheme {
            Surface(color = MeshColors.background) { PairScreen("", false) { _, _ -> } }
        } }
        val brand = compose.onNodeWithContentDescription("Buildmesh").assertIsDisplayed()
        compose.onNodeWithText("buildmesh").assertIsDisplayed()
        val pixels = brand.captureToImage().toPixelMap()
        val colours = (0 until pixels.height).flatMap { y ->
            (0 until pixels.width).map { x -> pixels[x, y].toArgb() }
        }.toSet()
        for (colour in listOf(0xff00d4ff.toInt(), 0xff22c55e.toInt(), 0xff8b5cf6.toInt())) {
            assertTrue("Missing a Relay node colour", colour in colours)
        }
        saveScreenshot("branding-pairing")
    }

    @Test fun homeShowsTheBrandAndDetailsKeepTheirTitle() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            server.enqueue(MockResponse().setResponseCode(204))
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            lateinit var vm: BuildmeshViewModel
            compose.runOnUiThread { vm = BuildmeshViewModel(app) }
            try {
                compose.setContent { BuildmeshTheme { BuildmeshApp(vm) } }
                compose.waitUntil(10000) { !vm.state.value.restoring }
                compose.onNodeWithContentDescription("Buildmesh").assertIsDisplayed()
                compose.onNodeWithText("buildmesh").assertIsDisplayed()
                saveScreenshot("branding-home")
                // Settings is a destination in the navigation bar, not an app-bar icon.
                compose.onNode(hasText("Settings") and hasAnyAncestor(hasTestTag("bottom-navigation"))).performClick()
                compose.onNodeWithText("Paired desktop").assertIsDisplayed()
            } finally {
                compose.runOnUiThread { vm.forget() }
                store.clear()
            }
        }
    }
}
