package dev.buildmesh.remote

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class PullRequestsTest {
    // GitHubPullRequestFeed and PrMergeability from src/types/generated.
    private val fixture = """{"items":[{"number":42,"title":"Fix search","body":"","url":"https://github.com/o/r/pull/42","state":"open","draft":false,
        "head_ref":"feature/search","head_repo_owner":"o","head_repo_clone_url":"https://github.com/o/r.git","head_sha":"abc","author":"o",
        "mergeable":true,"mergeable_state":"clean","checks_state":"success","review_decision":"approved"}],
        "completeness":{"returned":1,"pages_fetched":1,"complete":false,"incomplete_reason":"safety_cap","reported_total":1200}}"""
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
            server.enqueue(MockResponse().setBody("""{"mergeable":true,"mergeable_state":"clean"}"""))
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
            server.enqueue(MockResponse().setBody("""{"mergeable":true,"mergeable_state":"blocked"}"""))
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
            server.enqueue(MockResponse().setBody("""{"mergeable":true,"mergeable_state":"clean"}"""))
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
        val unknown = checkedMergeability(feed().items.single(), Json.parseToJsonElement("""{"mergeable":null,"mergeable_state":"unknown"}"""))
        assertFalse(unknown.canMerge)
    }
}
