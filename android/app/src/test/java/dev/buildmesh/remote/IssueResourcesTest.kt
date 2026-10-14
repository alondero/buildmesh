package dev.buildmesh.remote

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class IssueResourcesTest {
    private val feed = issueFeed(Json.parseToJsonElement("""{
        "items":[
            {"number":42,"title":"Fix deadline","body":"Details","url":"https://github.com/o/r/issues/42","state":"open","labels":["bug","buildmesh:run"],"author":"octo","blocked_by":[43,99]},
            {"number":43,"title":"Prerequisite","body":"","url":"","state":"open","labels":[],"author":"","blocked_by":[]}
        ],
        "completeness":{"returned":2,"pages_fetched":1,"complete":false,"incomplete_reason":"safety_cap","reported_total":120}
    }"""))

    @Test fun filteringUsesTitleNumberAndTagsWithoutDiscardingMetadata() {
        val issues = feed.arrayAt("items")
        for (query in listOf(" DEADLINE ", "BUG", "buildmesh:run", "#42")) {
            assertEquals(listOf(42L), filterIssues(issues, query).map { it.number("number") })
        }
        assertEquals(2, filterIssues(issues, "   ").size)
        assertTrue(filterIssues(issues, "nothing").isEmpty())
        assertEquals("Showing the first 2 of 120 issues. The read hit its page limit, so more exist.", issueCompletenessNote(feed))
    }

    @Test fun issueMetadataAndLoadedOpenBlockersAreRetained() {
        val issues = feed.arrayAt("items")
        assertEquals(listOf("bug", "buildmesh:run"), issueLabels(issues.first()))
        assertEquals("octo", issues.first().text("author"))
        assertEquals(listOf(43L), issueBlockers(issues.first(), issues))
        assertTrue(issueBlockers(issues.last(), issues).isEmpty())
    }

    @Test fun completeUnknownTotalAndLegacyFeedsDoNotImplyTheSameCompleteness() {
        val complete = issueFeed(Json.parseToJsonElement("""{"items":[],"completeness":{"returned":0,"pages_fetched":1,"complete":true,"incomplete_reason":null,"reported_total":null}}"""))
        assertNull(issueCompletenessNote(complete))
        val partial = issueFeed(Json.parseToJsonElement("""{"items":[],"completeness":{"returned":0,"pages_fetched":1,"complete":false,"incomplete_reason":"upstream_incomplete","reported_total":null}}"""))
        assertEquals("Showing the first 0 issues. GitHub reported that its own index was still incomplete — try again.", issueCompletenessNote(partial))
        assertEquals("Completeness is unavailable. More issues may exist.", issueCompletenessNote(issueFeed(Json.parseToJsonElement("[]"))))
    }
}
