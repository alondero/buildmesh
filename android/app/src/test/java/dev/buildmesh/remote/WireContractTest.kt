package dev.buildmesh.remote

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

// The bodies below are the canonical mobile-route fixtures, serialised from
// the Rust wire types (issue #2318). Hand-written fixtures in the old shapes
// kept passing while the desktop had moved on: the issues feed moving to
// `{items, completeness}` (issue #2024) broke the Issues screen with every
// Android test green.
class WireContractTest {
    /** The Spawn Menu the desktop sends, in the backend's own order. */
    private val menu = MobileWireFixtures.array("providers").map { it.jsonObject }

    /** A saved Launch Configuration row: its `id` is the configuration's id. */
    private val savedConfiguration = menu.single { it.text("id") == "launch/405f11ec" }

    /** A proxied Spawn Option: it has no configuration of its own. */
    private val proxiedOption = menu.single { it.text("id") == "claude:minimax" }

    /** The mesh row the desktop sends, with only its default varied. */
    private val meshRow = MobileWireFixtures.array("meshes").first().jsonObject

    @Test fun issuesFromTheCurrentDesktopFeedAreListed() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(MobileWireFixtures.raw("issues_feed")))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val issues = issueItems(api.get("/api/meshes/65/issues"))
                assertEquals(listOf(42L), issues.map { it.number("number") })
                assertEquals("Fix deadline", issues.single().text("title"))
            } finally { api.close() }
        }
    }

    @Test fun issuesFromADesktopBeforeTheFeedWrapperAreStillListed() {
        val legacy = Json.parseToJsonElement("""[{"number":7,"title":"Older desktop","body":"b"}]""")
        assertEquals(listOf(7L), issueItems(legacy).map { it.number("number") })
    }

    @Test fun aSavedLaunchConfigurationStartsAsItsSpawnOptionPlusTheConfigurationId() {
        assertEquals(
            MobileWireFixtures.obj("create_node_request_saved_configuration"),
            createNodeBody(3, savedConfiguration, "  Fix it \n", "op-1"),
        )
        assertEquals(
            MobileWireFixtures.obj("create_node_request_proxied"),
            createNodeBody(3, proxiedOption, " ", "op-1"),
        )
    }

    // Issue #2331 — the desktop binds `operation_key` to the node in the same
    // transaction as the INSERT, so one key per submission makes a retry
    // resolve to the same node instead of a duplicate.
    @Test fun everySubmissionCarriesItsOwnOperationKey() {
        assertNotEquals(newOperationKey(), newOperationKey())
        assertTrue(newOperationKey().isNotBlank())
    }

    // Issue #2331 — a committed-but-unstarted node must reach the user with
    // its identity, not be reported as "nothing was created".
    @Test fun aCommittedOutcomeAlwaysNamesItsNode() {
        val active = readCreateNodeOutcome(MobileWireFixtures.obj("create_node_outcome_active"))
        assertEquals(17L, (active as CreateNodeOutcome.Active).node.number("id"))

        val pending = readCreateNodeOutcome(MobileWireFixtures.obj("create_node_outcome_start_pending"))
        assertEquals(17L, (pending as CreateNodeOutcome.StartPending).node.number("id"))
        assertEquals("App not ready yet", pending.reason)

        val failed = readCreateNodeOutcome(MobileWireFixtures.obj("create_node_outcome_start_failed"))
        assertEquals(17L, (failed as CreateNodeOutcome.StartFailed).node.number("id"))
        assertEquals("claude CLI not found", failed.startError)
    }

    // An unrecognised discriminant must fail loudly: silently treating it as
    // "nothing created" is what invites the duplicate retry.
    @Test(expected = IllegalStateException::class)
    fun anUnknownOutcomeIsRejectedRatherThanTreatedAsNothingCreated() {
        readCreateNodeOutcome(Json.parseToJsonElement("""{"outcome":"from_the_future"}""").obj())
    }

    @Test fun archiveResumeOffersOnlySpawnOptionsItsRouteAccepts() {
        assertFalse(launchableWithoutConfiguration(savedConfiguration))
        assertTrue(launchableWithoutConfiguration(proxiedOption))
    }

    // Issue and PR spawns always carry the backend-built prompt, so they offer
    // only rows that can take one; Capture can also open a plain session.
    @Test fun spawnMenusKeepBackendOrderAndOfferSavedConfigurations() {
        assertEquals(listOf("launch/405f11ec", "claude:minimax"), launchOptions(menu, needsPrompt = true).map { it.text("id") })
        assertEquals(listOf("terminal", "launch/405f11ec", "claude:minimax"), launchOptions(menu, needsPrompt = false).map { it.text("id") })
    }

    // A harness without startup prefill still takes a prompt (the desktop types
    // it after startup); a desktop that predates the flag reports only prefill.
    @Test fun promptSupportReadsTypedPromptCapabilityWithPrefillFallback() {
        fun row(capabilities: String) = Json.parseToJsonElement("""{"capabilities":$capabilities}""").jsonObject
        assertTrue(supportsPrompt(row("""{"supports_prefill":false,"accepts_initial_prompt":true}""")))
        assertTrue(supportsPrompt(row("""{"supports_prefill":true}""")))
        assertFalse(supportsPrompt(row("""{"supports_prefill":false,"accepts_initial_prompt":false}""")))
    }

    // Mesh default sends no launch fields: the desktop resolves its own
    // explicit > mesh > app-wide chain, exactly as for a desktop spawn.
    @Test fun meshDefaultLeavesTheLaunchChoiceToTheDesktop() {
        assertEquals(
            MobileWireFixtures.obj("create_node_request_mesh_default"),
            createNodeBody(3, null, "Fix it", "op-1"),
        )
        val issue = issueItems(MobileWireFixtures.body("issues_feed")).single()
        assertEquals(MobileWireFixtures.obj("issue_spawn_request_mesh_default"), issueSpawnBody(issue, null))
        assertEquals(
            MobileWireFixtures.obj("issue_spawn_request_saved_configuration"),
            issueSpawnBody(issue, savedConfiguration),
        )
        assertEquals(
            MobileWireFixtures.obj("issue_spawn_request_proxied"),
            issueSpawnBody(issue, proxiedOption),
        )
    }

    // http::routes::pr::PullSpawnRequest parses this exact body
    // (`pull_spawn_forwards_the_phone_body_and_answers_the_created_node`).
    @Test fun aPullRequestSpawnSendsTheHeadTheDesktopProbeSends() {
        val pr = pullFeed(MobileWireFixtures.body("pulls_feed")).items.single()
        assertEquals(
            MobileWireFixtures.obj("pull_spawn_request_saved_configuration"),
            pullSpawnBody(pr, savedConfiguration),
        )
        assertEquals(
            MobileWireFixtures.obj("pull_spawn_request_mesh_default"),
            pullSpawnBody(pr, null),
        )
    }

    // A mesh's own default names its row; otherwise the app-wide default the
    // phone cannot see applies, so the control just says "Mesh default".
    @Test fun theMeshDefaultIsNamedOnlyWhenTheMeshPinsAMenuRow() {
        fun mesh(default: JsonElement) = JsonObject(meshRow + ("default_provider" to default))
        assertEquals("MiniMax via Claude Code", meshDefaultName(mesh(JsonPrimitive("claude:minimax")), menu))
        assertNull(meshDefaultName(mesh(JsonNull), menu))
        assertNull(meshDefaultName(mesh(JsonPrimitive("retired-harness")), menu))
        assertNull(meshDefaultName(null, menu))
    }

    // POST /api/meshes/{id}/pr requires node_id (http::routes::pr::CreatePrRequest);
    // a mesh-scoped body is rejected with 400.
    @Test fun aPullRequestIsCreatedFromTheNodeWorktreeThatWasPreviewed() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(MobileWireFixtures.raw("pr_source")))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val source = prSource(api.get("/api/meshes/3/pr/source?node_id=17"))
                assertEquals(PrSource(head = "topic", base = "main"), source)
                assertEquals(
                    MobileWireFixtures.obj("create_pr_request"),
                    pullRequestBody(17, " Native change ", "Explains the change", source, " main "),
                )
                assertEquals(
                    MobileWireFixtures.obj("create_pr_request_without_base"),
                    pullRequestBody(17, "Native change", "", source, " "),
                )
            } finally { api.close() }
        }
    }

    @Test fun aPullRequestSourceWithoutBranchesIsReportedRatherThanSubmitted() {
        val failure = runCatching { prSource(Json.parseToJsonElement("""{"head_branch":"","base_branch":"main"}""")) }.exceptionOrNull()
        assertTrue(failure is IllegalStateException)
    }

    @Test fun anUnexpectedIssuesShapeIsAReportableErrorNotACrash() {
        for (body in listOf("""{"error":"none"}""", "\"text\"", """{"items":{"number":1}}""")) {
            val failure = runCatching { issueItems(Json.parseToJsonElement(body)) }.exceptionOrNull()
            assertTrue("$body must fail as IllegalStateException, got $failure", failure is IllegalStateException)
            assertEquals(
                "The desktop sent an unexpected issues response. Update Buildmesh on the desktop or on this phone.",
                friendlyError(failure as Exception),
            )
        }
    }
}
