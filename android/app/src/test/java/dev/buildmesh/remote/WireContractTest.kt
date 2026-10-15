package dev.buildmesh.remote

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

// Fixtures follow the current Rust wire types (src/types/generated/), not the
// client's assumptions: hand-written fixtures in the old shapes kept passing
// while the desktop had moved on.
class WireContractTest {
    private val issueFeed = """{"items":[
        {"number":2313,"title":"Mobile issues crash","body":"","url":"https://github.com/o/r/issues/2313","state":"open","labels":["bug"],"blocked_by":[],"author":"octo"}
    ],"completeness":{"returned":1,"pages_fetched":1,"complete":true,"incomplete_reason":null,"reported_total":null}}"""

    @Test fun issuesFromTheCurrentDesktopFeedAreListed() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(issueFeed))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val issues = issueItems(api.get("/api/meshes/65/issues"))
                assertEquals(listOf(2313L), issues.map { it.number("number") })
                assertEquals("Mobile issues crash", issues.single().text("title"))
            } finally { api.close() }
        }
    }

    @Test fun issuesFromADesktopBeforeTheFeedWrapperAreStillListed() {
        val legacy = Json.parseToJsonElement("""[{"number":7,"title":"Older desktop","body":"b"}]""")
        assertEquals(listOf(7L), issueItems(legacy).map { it.number("number") })
    }

    // Shapes from agent::provider_menu: configuration_menu overwrites a saved
    // configuration row's `id` with the configuration id.
    private val savedConfiguration = Json.parseToJsonElement("""{"id":"launch/405f11ec","label":"Opus","harness_id":"claude","provider_id":null,
        "capabilities":{"supports_prefill":true},"configuration":{"id":"launch/405f11ec","spawn_option_id":"claude","name":"Opus"}}""").obj()
    private val proxiedOption = Json.parseToJsonElement("""{"id":"claude:minimax","label":"MiniMax via Claude Code","harness_id":"claude",
        "provider_id":"minimax","capabilities":{"supports_prefill":true}}""").obj()

    @Test fun aSavedLaunchConfigurationStartsAsItsSpawnOptionPlusTheConfigurationId() {
        assertEquals(
            Json.parseToJsonElement("""{"mesh_id":3,"provider":"claude","configuration_id":"launch/405f11ec","rows":24,"cols":80,"operation_key":"op-1","prompt":"Fix it"}"""),
            createNodeBody(3, savedConfiguration, "  Fix it \n", "op-1"),
        )
        assertEquals(
            Json.parseToJsonElement("""{"mesh_id":3,"provider":"claude:minimax","rows":24,"cols":80,"operation_key":"op-1"}"""),
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
        val node = Json.parseToJsonElement("""{"id":9,"mesh_id":3,"name":"idea","status":"idle"}""").obj()

        val active = readCreateNodeOutcome(
            Json.parseToJsonElement("""{"outcome":"active","node":$node,"operation_key":"op-1"}""").obj()
        )
        assertEquals(9L, (active as CreateNodeOutcome.Active).node.number("id"))

        val pending = readCreateNodeOutcome(
            Json.parseToJsonElement("""{"outcome":"start_pending","node":$node,"operation_key":"op-1","reason":"App not ready yet"}""").obj()
        )
        assertEquals(9L, (pending as CreateNodeOutcome.StartPending).node.number("id"))
        assertEquals("App not ready yet", pending.reason)

        val failed = readCreateNodeOutcome(
            Json.parseToJsonElement("""{"outcome":"start_failed","node":$node,"operation_key":"op-1","start_error":"claude CLI not found"}""").obj()
        )
        assertEquals(9L, (failed as CreateNodeOutcome.StartFailed).node.number("id"))
        assertEquals("claude CLI not found", failed.startError)
    }

    // An unrecognised discriminant must fail loudly: silently treating it as
    // "nothing created" is what invites the duplicate retry.
    @Test(expected = IllegalStateException::class)
    fun anUnknownOutcomeIsRejectedRatherThanTreatedAsNothingCreated() {
        readCreateNodeOutcome(Json.parseToJsonElement("""{"outcome":"from_the_future"}""").obj())
    }

    @Test fun issueAndArchiveLaunchesOfferOnlySpawnOptionsTheirRoutesAccept() {
        assertFalse(launchableWithoutConfiguration(savedConfiguration))
        assertTrue(launchableWithoutConfiguration(proxiedOption))
    }

    // POST /api/meshes/{id}/pr requires node_id (http::routes::pr::CreatePrRequest);
    // a mesh-scoped body is rejected with 400.
    @Test fun aPullRequestIsCreatedFromTheNodeWorktreeThatWasPreviewed() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("""{"head_branch":"gh2313-mobile-fix","base_branch":"trunk"}"""))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val source = prSource(api.get("/api/meshes/3/pr/source?node_id=17"))
                assertEquals(PrSource(head = "gh2313-mobile-fix", base = "trunk"), source)
                assertEquals(
                    Json.parseToJsonElement("""{"title":"Fix crash","body":"Details","node_id":17,"head_branch":"gh2313-mobile-fix","base_branch":"release"}"""),
                    pullRequestBody(17, " Fix crash ", "Details", source, " release "),
                )
                assertEquals(
                    Json.parseToJsonElement("""{"title":"Fix crash","body":"","node_id":17,"head_branch":"gh2313-mobile-fix"}"""),
                    pullRequestBody(17, "Fix crash", "", source, " "),
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
