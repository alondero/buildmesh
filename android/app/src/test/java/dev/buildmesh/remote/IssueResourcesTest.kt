package dev.buildmesh.remote

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class IssueResourcesTest {
    /** The canonical issue row, so a renamed field breaks these tests too. */
    private val issueRow = MobileWireFixtures.obj("issues_feed").arrayAt("items").first()

    /**
     * The canonical feed's envelope, with this test's own rows and
     * completeness: the triage screen needs a tagged, blocked issue and a
     * truncated feed.
     */
    private val feed = issueFeed(
        JsonObject(
            MobileWireFixtures.obj("issues_feed") +
                ("items" to JsonArray(listOf(
                    issue(
                        mapOf(
                            "number" to JsonPrimitive(42),
                            "title" to JsonPrimitive("Fix deadline"),
                            "body" to JsonPrimitive("Details"),
                            "url" to JsonPrimitive("https://github.com/o/r/issues/42"),
                            "state" to JsonPrimitive("open"),
                            "labels" to JsonArray(listOf(JsonPrimitive("bug"), JsonPrimitive("buildmesh:run"))),
                            "author" to JsonPrimitive("octo"),
                            "blocked_by" to JsonArray(listOf(JsonPrimitive(43), JsonPrimitive(99))),
                        ),
                    ),
                    issue(
                        mapOf(
                            "number" to JsonPrimitive(43),
                            "title" to JsonPrimitive("Prerequisite"),
                            "body" to JsonPrimitive(""),
                            "url" to JsonPrimitive(""),
                            "state" to JsonPrimitive("open"),
                            "labels" to JsonArray(emptyList()),
                            "author" to JsonPrimitive(""),
                            "blocked_by" to JsonArray(emptyList()),
                        ),
                    ),
                ))) +
                ("completeness" to completeness(
                    "returned" to JsonPrimitive(2),
                    "pages_fetched" to JsonPrimitive(1),
                    "complete" to JsonPrimitive(false),
                    "incomplete_reason" to JsonPrimitive("safety_cap"),
                    "reported_total" to JsonPrimitive(120),
                ))
        ),
    )

    /** The canonical issue row with this test's own values. */
    private fun issue(overrides: Map<String, JsonElement>) = JsonObject(issueRow + overrides)

    /** The canonical feed's completeness block with this test's own values. */
    private fun completeness(vararg overrides: Pair<String, JsonElement>) =
        JsonObject(MobileWireFixtures.obj("issues_feed").objectAt("completeness")!! + overrides)

    /** The canonical feed's envelope with this test's own rows and completeness. */
    private fun feedBody(items: JsonArray, completeness: JsonObject) =
        JsonObject(
            MobileWireFixtures.obj("issues_feed") +
                ("items" to items) +
                ("completeness" to completeness),
        )

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
        val complete = issueFeed(
            feedBody(
                JsonArray(emptyList()),
                completeness(
                    "returned" to JsonPrimitive(0),
                    "pages_fetched" to JsonPrimitive(1),
                    "complete" to JsonPrimitive(true),
                    "incomplete_reason" to JsonNull,
                    "reported_total" to JsonNull,
                ),
            ),
        )
        assertNull(issueCompletenessNote(complete))
        val partial = issueFeed(
            feedBody(
                JsonArray(emptyList()),
                completeness(
                    "returned" to JsonPrimitive(0),
                    "pages_fetched" to JsonPrimitive(1),
                    "complete" to JsonPrimitive(false),
                    "incomplete_reason" to JsonPrimitive("upstream_incomplete"),
                    "reported_total" to JsonNull,
                ),
            ),
        )
        assertEquals("Showing the first 0 issues. GitHub reported that its own index was still incomplete — try again.", issueCompletenessNote(partial))
        // A desktop older than the feed wrapper: a bare array, not the
        // current wire shape.
        assertEquals("Completeness is unavailable. More issues may exist.", issueCompletenessNote(issueFeed(Json.parseToJsonElement("[]"))))
    }
}
