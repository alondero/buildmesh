package dev.buildmesh.remote

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import dev.buildmesh.remote.wire.NodeCircuitRun
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import java.util.concurrent.ConcurrentLinkedQueue

class CircuitActionsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private fun withDesktop(respond: (RecordedRequest) -> MockResponse, body: (BuildmeshViewModel, ConcurrentLinkedQueue<RecordedRequest>) -> Unit) {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val store = SessionStore(app)
        store.clear()
        MockWebServer().use { server ->
            val requests = ConcurrentLinkedQueue<RecordedRequest>()
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    if (request.path == "/api/session") return MockResponse().setResponseCode(204)
                    requests.add(request)
                    return respond(request)
                }
            }
            server.start(java.net.InetAddress.getByName("127.0.0.1"), 0)
            store.save(DeviceSession(server.url("/").toString(), null, "bm_session=test; Path=/; HttpOnly"))
            lateinit var vm: BuildmeshViewModel
            compose.runOnUiThread { vm = BuildmeshViewModel(app) }
            try {
                compose.waitUntil(10000) { !vm.state.value.restoring }
                body(vm, requests)
            } finally { compose.runOnUiThread { vm.forget() } }
        }
    }

    @Test fun approvalNeedsConfirmationAndShowsARefusalInsideTheDialog() {
        val run = NodeCircuitRun.fromJson(Json.parseToJsonElement("""{"node_id":17,"run_id":7,"blueprint_name":"Team review","state":"running","step":null,"review":null,"needs_you":[{"kind":"approval","step":{"node_id":"gate","role":"Approval gate","attempt":1,"status":"blocked"},"detail":null,"actions":[],"agent_node_id":null}],"revision":12}"""))
        withDesktop({ MockResponse().setResponseCode(409).setBody("""{"error":"The run changed. Refresh before trying again."}""") }) { vm, requests ->
            compose.setContent { BuildmeshTheme { Column(Modifier.verticalScroll(rememberScrollState())) { CircuitRunPanel(run, vm, {}) } } }
            compose.onNodeWithText("Approve…").performScrollTo().performClick()
            assertTrue(requests.isEmpty())
            compose.onNodeWithText("Confirm").performClick()
            compose.waitUntil(10000) { !vm.state.value.busy && vm.state.value.error.isNotEmpty() }
            compose.onNodeWithText("The run changed. Refresh before trying again.").assertIsDisplayed()
            val post = requests.single()
            assertEquals("/api/circuit-runs/7/approve", post.path)
            assertEquals("""{"node_id":"gate"}""", post.body.readUtf8())
            assertTrue(vm.state.value.paired)
        }
    }

    @Test fun failedReviewChoicesStopLoadingAndCanBeRetried() {
        var attempts = 0
        withDesktop({
            attempts++
            if (attempts == 1) MockResponse().setResponseCode(500).setBody("""{"error":"Choices unavailable"}""")
            else MockResponse().setBody("""[{"circuit_id":null,"name":"Built-in Review Blueprint"}]""")
        }) { vm, _ ->
            compose.setContent { BuildmeshTheme { StartReviewButton(17, vm) } }
            compose.onNodeWithText("Start review…").performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Retry review choices").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Choices unavailable").assertIsDisplayed()
            compose.onNodeWithText("Loading review choices…").assertDoesNotExist()
            compose.onNodeWithText("Retry review choices").performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Built-in Review Blueprint", substring = true).fetchSemanticsNodes().isNotEmpty() }
            compose.onNode(hasText("Start review") and hasClickAction()).assertIsEnabled()
        }
    }
}
