package dev.buildmesh.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The listing screens' own refresh state.
 *
 * The pull owns its indicator: it starts with the gesture and ends when the
 * read that gesture asked for comes back. The state is deliberately counted
 * in reads rather than in answers, so a screen whose answer did not change
 * still retires its indicator, and a later poll cannot leave it spinning.
 */
class ListingRefreshTest {
    @Test fun thePullAsksForOneMoreReadThanTheOnesThatHaveLanded() {
        val refresh = ListingRefresh()
        assertFalse(refresh.refreshing)
        assertEquals(0, refresh.reload)

        refresh.pull()

        assertTrue(refresh.refreshing)
        assertEquals(1, refresh.reload)
    }

    @Test fun theReadThatLandsEndsThePullsIndicator() {
        val refresh = ListingRefresh()
        refresh.pull()

        refresh.settle()

        assertFalse(refresh.refreshing)
    }

    @Test fun anUnchangedAnswerStillEndsThePullsIndicator() {
        // The indicator belongs to the gesture, not to the data: a read that
        // answers with the same body still retires it.
        val refresh = ListingRefresh()
        refresh.pull()
        refresh.settle()

        refresh.pull()
        refresh.settle()

        assertFalse(refresh.refreshing)
    }

    @Test fun aReadThatLandsWithoutAPullDoesNotStartTheIndicator() {
        val refresh = ListingRefresh()

        refresh.settle()

        assertFalse(refresh.refreshing)
        assertEquals(0, refresh.reload)
    }

    @Test fun theInListRetryAsksForAReadWithoutStartingThePullIndicator() {
        val refresh = ListingRefresh()

        refresh.request()

        assertEquals(1, refresh.reload)
        assertFalse(refresh.refreshing)
    }

    @Test fun aSecondPullAfterALandedReadAsksForOneMoreRead() {
        val refresh = ListingRefresh()
        refresh.pull()
        refresh.settle()

        refresh.pull()

        assertTrue(refresh.refreshing)
        assertEquals(2, refresh.reload)

        refresh.settle()

        assertFalse(refresh.refreshing)
    }

    @Test fun aPollThatLandsAfterAPullDoesNotLeaveTheIndicatorSpinning() {
        // The pull asked for one read; a later poll read must not push the
        // counter past the pull and leave the screen waiting forever.
        val refresh = ListingRefresh()
        refresh.pull()
        refresh.settle()

        refresh.settle()

        assertFalse(refresh.refreshing)
    }

    @Test fun aPullThatReadsMoreThanOneRouteWaitsForAllOfThem() {
        // The Changes screen reads the changed files and the branch. The
        // indicator belongs to the whole refresh, so it stays on until the
        // last of the reads comes back.
        val refresh = ListingRefresh()

        refresh.pull(reads = 2)

        assertTrue(refresh.refreshing)
        assertEquals(1, refresh.reload)

        refresh.settle()

        assertTrue("The first route is not the whole refresh", refresh.refreshing)

        refresh.settle()

        assertFalse(refresh.refreshing)
    }

    @Test fun aReadThatLandsTwiceDoesNotEndTheIndicatorEarly() {
        // The screen's counter is capped by the pull, so a read that lands
        // more than once cannot retire an indicator that another route still
        // owes.
        val refresh = ListingRefresh()
        refresh.pull(reads = 2)

        refresh.settle()
        refresh.settle()
        refresh.settle()

        assertFalse(refresh.refreshing)
    }
}
