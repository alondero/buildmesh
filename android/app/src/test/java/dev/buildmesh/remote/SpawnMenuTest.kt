package dev.buildmesh.remote

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class SpawnMenuTest {
    private val native get() = JsonObject(MobileWireFixtures.array("providers").map { it.obj() }
        .single { it.text("id") == "codex" }.filterKeys { it != "unavailable_reason" } +
        mapOf("label" to JsonPrimitive("Codex")))

    private fun configuration(id: String, login: String? = null, route: Boolean = false) = buildJsonObject {
        native.forEach { (key, value) -> if (key != "logins") put(key, value) }
        put("id", id); put("label", id)
        put("configuration", buildJsonObject {
            put("id", id); put("name", id); put("spawn_option_id", if (route) "codex:proxy" else "codex")
            login?.let { put("login_id", it) }
        })
    }

    @Test fun loginChoicesRespectPinsRoutesAndUnsignedAccounts() {
        val options = launchOptions(listOf(native, configuration("launch/free"), configuration("launch/work", "work"),
            configuration("launch/route", route = true)), needsPrompt = true)
        assertEquals(listOf("default", "work", "guest"), options.filter { it.objectAt("configuration")?.text("id") == "launch/free" }.map { it.text("login_id") })
        assertEquals(listOf("work"), options.filter { it.objectAt("configuration")?.text("id") == "launch/work" }.map { it.text("login_id") })
        assertEquals(listOf("default"), options.filter { it.objectAt("configuration")?.text("id") == "launch/route" }.map { it.text("login_id") })
        assertTrue(options.first { it.text("login_id") == "guest" }.text("unavailable_reason").contains("Not signed in"))
        assertEquals("", options.first { it.text("login_id") == "default" }.text("unavailable_reason"))
    }

    @Test fun selectedLoginReachesCreateIssuePullAndReviewerBodies() {
        val chosen = launchOptions(listOf(native), needsPrompt = true).single { it.text("login_id") == "work" }
        assertEquals("[\"codex\",\"work\"]", chosen.text("id"))
        val create = createNodeBody(3, chosen, "Fix it", "op-1")
        assertEquals(MobileWireFixtures.obj("create_node_request_login"), create)
        val issue = issueSpawnBody(MobileWireFixtures.obj("issues_feed")["items"]!!.jsonArray.first().obj(), chosen)
        assertEquals("work", issue.text("login_id"))
        assertEquals("codex", issue.text("provider"))
        val pull = pullSpawnBody(pullFeed(MobileWireFixtures.obj("pulls_feed")).items.first(), chosen)
        assertEquals("work", pull.text("login_id"))
        assertEquals("codex", pull.text("provider"))
        assertEquals("work", startReviewRequest(null, 3, "codex", "work").toJson().text("reviewer_login_id"))
        assertFalse(startReviewRequest(9, 3, "codex", "work").toJson().containsKey("reviewer_login_id"))
    }
}
