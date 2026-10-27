package dev.buildmesh.remote

import org.junit.Assert.*
import org.junit.Test

/**
 * The terminal's own quick-key row, and the approval pair above it (issue #2609).
 *
 * The two used to offer the same keys: a bare `y` in the quick-key row could
 * approve whichever prompt the agent happened to show next, while the band
 * above the terminal names the request it answers.
 */
class TerminalQuickKeysTest {
    @Test fun theApprovalKeysAreNotDuplicatedInTheQuickKeyRow() {
        val labels = terminalQuickKeys.map { it.first }
        assertFalse("A bare approval key cannot name the request it would answer", labels.contains("y"))
        assertFalse("A bare approval key cannot name the request it would answer", labels.contains("n"))
        assertEquals(listOf("Esc", "Tab", "Shift+Tab", "Ctrl+C", "Enter", "↑", "↓", "←", "→"), labels)
    }

    @Test fun theApprovalPairSendsTheDesktopKeystrokes() {
        assertEquals("y\r", ALLOW_SEQUENCE)
        assertEquals("n\r", DENY_SEQUENCE)
    }
}
