package dev.buildmesh.remote

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import kotlinx.serialization.json.Json
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class NativeScreensTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test fun pairingUsesNativeInputsAndDeliversTheWholeInvitation() {
        var received = ""
        compose.setContent { BuildmeshTheme { PairScreen("", false) { url, _ -> received = url } } }
        compose.onNodeWithText("Pair with desktop").assertIsNotEnabled()
        compose.onNodeWithText("Pairing URL").performTextInput("https://desktop:1992/#pair=invitation&ca=pin")
        compose.onNodeWithText("Pair with desktop").performScrollTo().performClick()
        compose.runOnIdle { assertEquals("https://desktop:1992/#pair=invitation&ca=pin", received) }
    }

    @Test fun dashboardOpensNodesAndFiltersAttention() {
        val mesh = Json.parseToJsonElement("{\"id\":2,\"name\":\"Test mesh\",\"path\":\"/repo\"}").obj()
        val ready = Json.parseToJsonElement("{\"id\":3,\"mesh_id\":2,\"name\":\"Ready agent\",\"branch\":\"main\",\"status\":\"ready\"}").obj()
        val waiting = Json.parseToJsonElement("{\"id\":4,\"mesh_id\":2,\"name\":\"Waiting agent\",\"branch\":\"task\",\"status\":\"awaiting_input\"}").obj()
        var opened = 0L
        compose.setContent { BuildmeshTheme { Dashboard(RemoteState(nodes = listOf(ready, waiting), meshes = listOf(mesh)), "Work", 0, {}, { opened = it }, { _, _ -> }) } }
        compose.onAllNodesWithText("Needs attention", substring = false).onFirst().performClick()
        compose.onNodeWithText("Ready agent").assertDoesNotExist()
        compose.onNodeWithText("Waiting agent").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(4L, opened) }
    }

    @Test fun encryptedSessionRestoresAndForgetRemovesIt() {
        val store = SessionStore(compose.activity)
        store.clear()
        val session = DeviceSession("https://desktop:1992", byteArrayOf(1, 2, 3), "bm_session=private; Path=/; HttpOnly; Secure")
        try {
            store.save(session)
            val bytes = compose.activity.filesDir.resolve("device-session").readBytes()
            assertFalse(bytes.toString(Charsets.UTF_8).contains("private"))
            assertEquals(session.origin, store.read()!!.origin)
            assertEquals(session.cookie, store.read()!!.cookie)
            assertArrayEquals(session.root, store.read()!!.root)
            store.clear()
            assertNull(store.read())
        } finally { store.clear() }
    }
}
