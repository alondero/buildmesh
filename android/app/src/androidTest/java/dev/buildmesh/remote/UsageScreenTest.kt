package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.test.core.app.ApplicationProvider
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import java.util.concurrent.TimeUnit

class UsageScreenTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test fun theAppBarRefreshForcesOneFreshReadAndTheNextForegroundReadIsNotForced() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        SessionStore(app).clear()
        lateinit var vm: BuildmeshViewModel
        lateinit var owner: LifecycleOwner
        lateinit var registry: LifecycleRegistry
        compose.runOnUiThread {
            vm = BuildmeshViewModel(app)
            owner = object : LifecycleOwner { override val lifecycle: Lifecycle get() = registry }
            registry = LifecycleRegistry(owner).apply { currentState = Lifecycle.State.RESUMED }
        }
        MockWebServer().use { server ->
            repeat(3) { server.enqueue(MockResponse().setBody(MobileWireFixtures.raw("usage"))) }
            val api = BuildmeshApi(server.url("/").toString())
            try {
                compose.setContent { BuildmeshTheme {
                    CompositionLocalProvider(LocalLifecycleOwner provides owner) { UsageScreen(api, vm) }
                } }
                compose.waitUntil(5000) { compose.onAllNodesWithText("Claude subscription").fetchSemanticsNodes().isNotEmpty() }
                assertEquals("/api/usage?refresh=false", server.takeRequest(5, TimeUnit.SECONDS)?.path)
                // The app-bar action is the shared recovery affordance, and a
                // read the user asked for reads past the desktop's cache.
                compose.runOnUiThread { vm.retry() }
                compose.waitForIdle()
                assertEquals("/api/usage?refresh=true", server.takeRequest(5, TimeUnit.SECONDS)?.path)
                compose.waitForIdle()
                compose.runOnUiThread { registry.currentState = Lifecycle.State.CREATED }
                compose.waitForIdle()
                compose.runOnUiThread { registry.currentState = Lifecycle.State.RESUMED }
                compose.waitForIdle()
                assertEquals("/api/usage?refresh=false", server.takeRequest(5, TimeUnit.SECONDS)?.path)
            } finally {
                compose.runOnUiThread { registry.currentState = Lifecycle.State.DESTROYED; vm.forget() }
                api.close()
            }
        }
    }
}
