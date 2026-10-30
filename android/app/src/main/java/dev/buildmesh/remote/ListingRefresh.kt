package dev.buildmesh.remote

import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier

/**
 * A listing screen's own refresh state.
 *
 * `reload` is the screen's own counter: the pull gesture and the in-list
 * Retry move it, and the screen re-reads its own route when it changes. The
 * app-bar action is a separate, shared surface — it re-reads through the
 * ViewModel's `resourceRevision` and keeps the thin progress bar — so the two
 * controls never do one job twice.
 *
 * The pull owns its own indicator. `wanted` counts the reads a pull asked for
 * and `landed` counts the reads that came back, so the indicator ends with
 * the read the gesture started rather than with the gesture itself: a screen
 * whose answer did not change still retires its indicator, and a later poll
 * cannot leave it spinning. A screen that reads more than one route tells the
 * pull how many it re-reads, so the indicator covers all of them.
 */
@Stable
class ListingRefresh {
    /** The screen's own reload counter. */
    var reload by mutableIntStateOf(0)
        private set
    private var wanted by mutableIntStateOf(0)
    private var landed by mutableIntStateOf(0)

    /** Whether the pull's own indicator belongs on screen. */
    val refreshing: Boolean get() = wanted > landed

    /** Ask the screen to read its own route again (the in-list Retry). */
    fun request() {
        reload++
    }

    /**
     * The pull gesture: one more read than the ones that have landed.
     *
     * The read itself is the screen's own counter moving, so a pull re-reads
     * the screen's route once and nothing else. `reads` is how many routes
     * the gesture re-reads; the indicator stays on until all of them have
     * come back.
     */
    fun pull(reads: Int = 1) {
        wanted = landed + reads
        reload++
    }

    /** Record that a read came back, successfully or not. */
    fun settle() {
        if (landed < wanted) landed++
    }
}

/**
 * A listing screen's scroller under the pull gesture.
 *
 * The gesture re-reads the screen's own route and owns the indicator it
 * shows; the app-bar action keeps the shared thin progress bar, so one
 * gesture never drives two indicators. `reads` is how many routes the
 * gesture re-reads.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PullToRefreshList(refresh: ListingRefresh, modifier: Modifier = Modifier, reads: Int = 1, content: @Composable () -> Unit) {
    PullToRefreshBox(
        isRefreshing = refresh.refreshing,
        onRefresh = { refresh.pull(reads) },
        modifier = modifier,
    ) { content() }
}
