package dev.buildmesh.remote

import android.app.Activity
import android.app.Application
import android.content.ActivityNotFoundException
import android.content.Intent
import android.graphics.Bitmap
import android.speech.RecognizerIntent
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivityResultRegistryOwner
import androidx.activity.result.ActivityResultRegistry
import androidx.activity.result.ActivityResultRegistryOwner
import androidx.activity.result.contract.ActivityResultContract
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.material3.Surface
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.core.app.ActivityOptionsCompat
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.json.jsonObject
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class TaskSpeechInputTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    // Exercise the production activity contract and result callback without
    // requiring a particular speech provider or recording audio in the suite.
    private class SpeechRegistry : ActivityResultRegistry(), ActivityResultRegistryOwner {
        override val activityResultRegistry: ActivityResultRegistry get() = this
        var intent: Intent? = null
        var requestCode = 0
        var failure: RuntimeException? = null
        override fun <I, O> onLaunch(requestCode: Int, contract: ActivityResultContract<I, O>, input: I, options: ActivityOptionsCompat?) {
            failure?.let { throw it }
            this.requestCode = requestCode
            intent = contract.createIntent(ApplicationProvider.getApplicationContext(), input)
        }
        fun complete(code: Int = Activity.RESULT_OK, words: ArrayList<String>? = arrayListOf("Fix the tests")) {
            dispatchResult(requestCode, code, Intent().putStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS, words))
        }
    }

    private fun withTask(draft: String = "", busy: Boolean = false, body: (BuildmeshViewModel, SpeechRegistry, StateRestorationTester) -> Unit) {
        val app = ApplicationProvider.getApplicationContext<Application>()
        app.getSharedPreferences("drafts", 0).edit().clear().commit()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse =
                    if (request.path == "/api/session") MockResponse().setResponseCode(204)
                    else MockResponse().setResponseCode(500)
            }
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            lateinit var vm: BuildmeshViewModel
            compose.runOnUiThread { vm = BuildmeshViewModel(app) }
            compose.waitUntil(10000) { vm.state.value.paired && !vm.state.value.restoring }
            compose.runOnUiThread { vm.saveDraft("capture", draft) }
            val registry = SpeechRegistry()
            val meshes = MobileWireFixtures.array("meshes").map { it.jsonObject }
            val providers = MobileWireFixtures.array("providers").map { it.jsonObject }
            val restoration = StateRestorationTester(compose)
            try {
                restoration.setContent {
                    CompositionLocalProvider(LocalActivityResultRegistryOwner provides registry) {
                        BuildmeshTheme {
                            val state by vm.state.collectAsState()
                            Surface(color = MeshColors.background) {
                                TaskScreen(state.copy(meshes = meshes, providers = providers, busy = busy), vm, meshes.first().number("id"), {}, "", {}, {})
                            }
                        }
                    }
                }
                body(vm, registry, restoration)
                // Dictation never starts an agent or sends any other request.
                assertEquals(1, server.requestCount)
            } finally { compose.runOnUiThread { vm.forget() } }
        }
    }

    @Test fun microphoneOpensNativeSpeechAndAppendsToTheSavedEditableDraft() = withTask("Investigate:") { vm, registry, _ ->
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        assertEquals(RecognizerIntent.ACTION_RECOGNIZE_SPEECH, registry.intent?.action)
        assertEquals(RecognizerIntent.LANGUAGE_MODEL_FREE_FORM, registry.intent?.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL))
        compose.onNodeWithContentDescription("Dictate task").assertIsNotEnabled()
        compose.onNodeWithText("Start agent").performScrollTo().assertIsNotEnabled()
        compose.runOnUiThread { registry.complete(words = arrayListOf(" Fix the tests ", "Other possibility")) }
        compose.onNodeWithText("Investigate: Fix the tests").assertExists()
        assertEquals("Investigate: Fix the tests", vm.draft("capture"))
        compose.onNodeWithText("Start agent").assertIsEnabled()
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().assertIsDisplayed()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val screenshot = instrumentation.uiAutomation.takeScreenshot() ?: error("No native screenshot")
        try {
            instrumentation.targetContext.cacheDir.resolve("task-dictation.png").outputStream().use {
                assertTrue(screenshot.compress(Bitmap.CompressFormat.PNG, 100, it))
            }
        } finally { screenshot.recycle() }
        compose.onNodeWithText("What should the agent do?").performScrollTo().performTextReplacement("Investigate: Fix the tests please")
        assertEquals("Investigate: Fix the tests please", vm.draft("capture"))
    }

    @Test fun repeatedDictationFillsAnEmptyDraftAndPreservesExistingLineBreaks() = withTask { vm, registry, _ ->
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.runOnUiThread { registry.complete() }
        compose.onNodeWithText("Fix the tests").assertExists()
        compose.onNodeWithText("What should the agent do?").performTextReplacement("Fix the tests\n")
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.runOnUiThread { registry.complete(words = arrayListOf("Then review")) }
        compose.onNodeWithText("Fix the tests\nThen review").assertExists()
        assertEquals("Fix the tests\nThen review", vm.draft("capture"))
    }

    @Test fun cancellationAndEmptyResultsPreserveTheDraftAndAllowRetry() = withTask("Keep this") { vm, registry, _ ->
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.runOnUiThread { registry.complete(Activity.RESULT_CANCELED) }
        compose.onNodeWithText("Keep this").assertExists()
        compose.onNodeWithContentDescription("Dictate task").assertIsEnabled().performClick()
        compose.runOnUiThread { registry.complete(words = arrayListOf("  ")) }
        compose.onNodeWithText("No speech was recognised", substring = true).assertExists()
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.runOnUiThread { registry.complete(words = null) }
        compose.onNodeWithText("Keep this").assertExists()
        assertEquals("Keep this", vm.draft("capture"))
        compose.onNodeWithContentDescription("Dictate task").assertIsEnabled()
    }

    @Test fun missingSpeechProviderAndDeniedLaunchShowRecoveryWithoutLosingText() = withTask("Keep this") { vm, registry, _ ->
        registry.failure = ActivityNotFoundException()
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.onNodeWithText("Speech input isn't available", substring = true).assertExists()
        registry.failure = SecurityException()
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.onNodeWithText("Speech input couldn't open", substring = true).assertExists()
        compose.onNodeWithContentDescription("Dictate task").assertIsEnabled()
        compose.onNodeWithText("Start agent").performScrollTo().assertIsEnabled()
        assertEquals("Keep this", vm.draft("capture"))
    }

    @Test fun submittingDisablesTheMicrophone() = withTask(busy = true) { _, registry, _ ->
        compose.onNodeWithContentDescription("Dictate task").assertIsNotEnabled()
        assertNull(registry.intent)
    }

    @Test fun resultAfterForgettingTheDesktopCannotChangeItsDraft() = withTask("Keep this") { vm, registry, _ ->
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        compose.runOnUiThread { vm.forget(); registry.complete() }
        compose.onNodeWithText("Keep this").assertExists()
        assertEquals("Keep this", vm.draft("capture"))
    }

    @Test fun pendingDictationSurvivesSavedStateRestoration() = withTask("Keep this") { vm, registry, restoration ->
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        restoration.emulateSavedInstanceStateRestore()
        compose.onNodeWithContentDescription("Dictate task").assertIsNotEnabled()
        compose.runOnUiThread { registry.complete() }
        compose.onNodeWithText("Keep this Fix the tests").assertExists()
        assertEquals("Keep this Fix the tests", vm.draft("capture"))
    }

    @Test fun lateResultCannotEnterADifferentPairedDesktopsDraft() = withTask("Private task") { vm, registry, _ ->
        val originalNamespace = vm.state.value.draftNamespace
        compose.onNodeWithContentDescription("Dictate task").performScrollTo().performClick()
        MockWebServer().use { other ->
            other.enqueue(MockResponse().setResponseCode(204).addHeader("Set-Cookie", "bm_session=other; Path=/; HttpOnly"))
            other.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            val invitation = "${other.url("/")}#pair=${"a".repeat(64)}"
            compose.runOnUiThread { vm.pair(invitation, "") }
            compose.waitUntil(10000) { vm.state.value.paired && !vm.state.value.busy && vm.state.value.draftNamespace != originalNamespace }
            compose.onNodeWithText("What should the agent do?").performTextReplacement("Other desktop task")
            compose.runOnUiThread { registry.complete() }
            compose.onNodeWithText("Other desktop task").assertExists()
            assertEquals("Other desktop task", vm.draft("capture"))
        }
    }
}
