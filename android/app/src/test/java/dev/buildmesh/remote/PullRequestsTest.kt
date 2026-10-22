package dev.buildmesh.remote

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class PullRequestsTest {
    /** The canonical pull request row, so a renamed field breaks these tests too. */
    private val pullRow = MobileWireFixtures.obj("pulls_feed").arrayAt("items").first()

    /**
     * The canonical feed's envelope, with this test's own row and
     * completeness: this test needs a pull request that is ready to merge and
     * a feed that was truncated at the page limit.
     */
    private val fixture = JsonObject(
        MobileWireFixtures.obj("pulls_feed") +
            ("items" to JsonArray(listOf(JsonObject(
                pullRow +
                    ("number" to JsonPrimitive(42)) +
                    ("title" to JsonPrimitive("Fix search")) +
                    ("url" to JsonPrimitive("https://github.com/o/r/pull/42")) +
                    ("head_ref" to JsonPrimitive("feature/search")) +
                    ("head_repo_owner" to JsonPrimitive("o")) +
                    ("head_repo_clone_url" to JsonPrimitive("https://github.com/o/r.git")) +
                    ("head_sha" to JsonPrimitive("abc")) +
                    ("author" to JsonPrimitive("o")) +
                    ("mergeable" to JsonPrimitive(true)) +
                    ("mergeable_state" to JsonPrimitive("clean")) +
                    ("checks_state" to JsonPrimitive("success")) +
                    ("review_decision" to JsonPrimitive("approved"))
            )))) +
            ("completeness" to JsonObject(
                MobileWireFixtures.obj("pulls_feed").objectAt("completeness")!!.mapValues { (key, value) ->
                    when (key) {
                        "returned" -> JsonPrimitive(1)
                        "pages_fetched" -> JsonPrimitive(1)
                        "complete" -> JsonPrimitive(false)
                        "incomplete_reason" -> JsonPrimitive("safety_cap")
                        "reported_total" -> JsonPrimitive(1200)
                        else -> value
                    }
                },
            ))
    ).toString()

    /** The canonical mergeability body with this test's own state. */
    private fun mergeability(mergeable: JsonElement, state: String) =
        JsonObject(
            MobileWireFixtures.obj("pr_mergeability") +
                ("mergeable" to mergeable) +
                ("mergeable_state" to JsonPrimitive(state)),
        ).toString()

    private fun feed() = pullFeed(Json.parseToJsonElement(fixture))

    @Test fun theGeneratedFeedCarriesChecksReviewsAndTruncation() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(fixture))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val feed = pullFeed(api.get("/api/meshes/3/pulls?state=open"))
                val pr = nodePulls(feed, "feature/search").single()
                assertEquals("success", pr.checks); assertEquals("approved", pr.review)
                assertTrue(pr.canMerge); assertFalse(feed.complete)
                assertEquals(1L, feed.returned); assertEquals(1200L, feed.total); assertEquals("safety_cap", feed.reason)
                assertEquals("/api/meshes/3/pulls?state=open", server.takeRequest().path)
            } finally { api.close() }
        }
    }

    @Test fun nodeMatchingExcludesBaseBranchesAndUnrelatedForks() {
        val pr = feed().items.single()
        val data = feed().copy(items = listOf(pr, pr.copy(number = 9, head = "main"), pr.copy(number = 10, headRepo = "https://github.com/fork/r.git")))
        assertEquals(listOf(42L), nodePulls(data, "feature/search").map { it.number })
        assertTrue(nodePulls(data, "").isEmpty())
    }

    @Test fun unknownConflictingDraftOrPolicyBlockedPullsCannotMerge() {
        val pr = feed().items.single()
        listOf(pr.copy(draft = true), pr.copy(mergeable = null), pr.copy(mergeable = false),
            pr.copy(mergeState = "blocked"), pr.copy(mergeState = "behind"), pr.copy(mergeState = "unknown"),
            pr.copy(checks = "failure"), pr.copy(checks = "pending"), pr.copy(review = "changes_requested"),
            pr.copy(review = "review_required")).forEach { assertFalse(it.toString(), it.canMerge) }
    }

    @Test fun aConfirmedSquashRechecksThenSubmitsTheExactUrl() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(mergeability(JsonPrimitive(true), "clean")))
            server.enqueue(MockResponse().setBody("""{"url":"https://github.com/o/r/pull/42","pr_number":42}"""))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                api.squashNodePull(3, feed().items.single())
                assertEquals("/api/meshes/3/pulls/42/mergeability", server.takeRequest().path)
                val request = server.takeRequest()
                assertEquals("POST", request.method)
                assertEquals("/api/meshes/3/pulls/42/merge", request.path)
                assertEquals("""{"url":"https://github.com/o/r/pull/42"}""", request.body.readUtf8())
            } finally { api.close() }
        }
    }

    @Test fun aFreshPolicyBlockPreventsTheMergePost() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(mergeability(JsonPrimitive(true), "blocked")))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val failure = runCatching { api.squashNodePull(3, feed().items.single()) }.exceptionOrNull()
                assertTrue(failure is IllegalStateException)
                assertEquals(1, server.requestCount)
            } finally { api.close() }
        }
    }

    @Test fun aRejectedMergePropagatesItsFailureInsteadOfAcknowledgingSuccess() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(mergeability(JsonPrimitive(true), "clean")))
            server.enqueue(MockResponse().setResponseCode(500).setBody("""{"error":"Branch protection"}"""))
            val api = BuildmeshApi(server.url("/").toString())
            try {
                val failure = runCatching { api.squashNodePull(3, feed().items.single()) }.exceptionOrNull()
                assertTrue(failure is ApiException)
                assertEquals("Branch protection", failure?.message)
            } finally { api.close() }
        }
    }

    @Test fun missingOrMalformedFeedAndMergeabilityAreReportableErrors() {
        listOf("{}", "[]", """{"items":[],"completeness":{}}""").forEach {
            assertTrue(runCatching { pullFeed(Json.parseToJsonElement(it)) }.exceptionOrNull() is IllegalStateException)
        }
        assertTrue(runCatching { checkedMergeability(feed().items.single(), Json.parseToJsonElement("{}")) }.exceptionOrNull() is IllegalStateException)
        val unknown = checkedMergeability(feed().items.single(), Json.parseToJsonElement(mergeability(JsonNull, "unknown")))
        assertFalse(unknown.canMerge)
    }
}
