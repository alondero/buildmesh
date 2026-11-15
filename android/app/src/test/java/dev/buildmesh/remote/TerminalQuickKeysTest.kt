package dev.buildmesh.remote

import org.junit.Assert.*
import org.junit.Test

/**
 * The keys under the Android terminal.
 *
 * A phone keyboard has no Esc, Tab, arrows or Ctrl. Harness CLIs are driven
 * by those keys, and by a few chords a fixed button list cannot keep up with:
 * Shift+Tab cycles mode in Claude Code and Codex and cycles agents in
 * OpenCode; Shift+Left and Shift+Right are the keys Codex shows for answering
 * a question; Ctrl+J inserts a newline in all three because Enter submits.
 * The row keeps the base keys and two one-shot modifiers, so a chord is
 * modifier then key. Approval letters stay off this row.
 */
class TerminalQuickKeysTest {
    private val normal = TerminalInputModes()
    private val appCursor = TerminalInputModes(applicationCursorKeys = true)

    private fun key(label: String) = terminalQuickKeys.first { it.label == label }

    private fun press(latch: QuickModifierLatch, label: String, modes: TerminalInputModes = normal) =
        latch.sequence(key(label), modes)

    @Test fun theRowKeepsOnlyKeysThePhoneKeyboardCannotSend() {
        assertEquals(
            listOf("Esc", "Ctrl+C", "Tab", "↑", "↓", "←", "→", "Enter"),
            terminalQuickKeys.map { it.label },
        )
        assertEquals(listOf("Shift", "Ctrl"), quickModifierLabels)
        val labels = terminalQuickKeys.map { it.label } + quickModifierLabels
        assertFalse(labels.any { it.contains("+") && it != "Ctrl+C" })
        assertFalse(labels.contains("y"))
        assertFalse(labels.contains("n"))
    }

    @Test fun shiftThenLeftAnswersACodexQuestion() {
        val latch = QuickModifierLatch()
        latch.toggleShift()
        // Modified arrows use the CSI form even when the agent is in
        // application-cursor mode. That is the sequence Codex binds.
        assertEquals("\u001b[1;2D", press(latch, "←", appCursor))
        assertEquals("\u001bOD", press(latch, "←", appCursor))
        latch.toggleShift()
        assertEquals("\u001b[1;2C", press(latch, "→"))
    }

    @Test fun shiftThenTabCyclesTheHarnessMode() {
        val latch = QuickModifierLatch()
        latch.toggleShift()
        assertEquals("⇧Tab", key("Tab").caption(shift = true, ctrl = false))
        assertEquals("\u001b[Z", press(latch, "Tab", appCursor))
        assertEquals("\t", press(latch, "Tab"))
    }

    @Test fun ctrlThenJInsertsANewlineAndCtrlCStaysOneTap() {
        val latch = QuickModifierLatch()
        latch.toggleCtrl()
        assertEquals("\n", latch.onCommit("j"))
        assertEquals("j", latch.onCommit("j"))

        latch.toggleCtrl()
        assertEquals("\u0018", latch.onCommit("x"))
        assertEquals("n", latch.onCommit("n"))

        latch.toggleCtrl()
        assertEquals("\u001b[1;5D", press(latch, "←"))

        latch.toggleShift()
        assertEquals("\u0003", press(latch, "Ctrl+C", appCursor))
        assertFalse(latch.shiftOn)
    }

    @Test fun aModifierAppliesOnceAndASecondTapCancelsIt() {
        val latch = QuickModifierLatch()
        latch.toggleShift()
        latch.toggleShift()
        assertFalse(latch.shiftOn)
        assertEquals("\u001b[D", press(latch, "←"))

        latch.toggleShift()
        latch.toggleCtrl()
        assertEquals("⇧^←", key("←").caption(shift = true, ctrl = true))
        assertEquals("\u001b[1;6D", press(latch, "←"))
        assertFalse(latch.shiftOn)
        assertFalse(latch.ctrlOn)

        // Enter submits. Shift does not turn it into a newline.
        latch.toggleShift()
        assertEquals("Enter", key("Enter").caption(shift = true, ctrl = false))
        assertEquals("\r", press(latch, "Enter"))
        assertFalse(latch.shiftOn)
        // Enter from the soft keyboard submits too, and drops a modifier
        // that was still armed.
        latch.toggleCtrl()
        assertEquals("\r", latch.onSubmit())
        assertFalse(latch.ctrlOn)

        // A held-back correction is not the chord's character.
        latch.toggleCtrl()
        assertEquals("\u007f\u007f", latch.onCommit("\u007f\u007f"))
        assertTrue(latch.ctrlOn)
        assertEquals("", latch.onCommit(""))
        assertTrue(latch.ctrlOn)
        assertEquals("\u007f\n", latch.onCommit("\u007fj"))
        assertFalse(latch.ctrlOn)
    }

    @Test fun plainArrowsStillFollowTheAgentsCursorMode() {
        val latch = QuickModifierLatch()
        assertEquals("\u001b[A", press(latch, "↑"))
        assertEquals("\u001bOA", press(latch, "↑", appCursor))
        assertEquals("Ctrl+C", key("Ctrl+C").caption(shift = true, ctrl = true))
    }

    @Test fun theApprovalPairSendsTheDesktopKeystrokes() {
        assertEquals("y\r", ALLOW_SEQUENCE)
        assertEquals("n\r", DENY_SEQUENCE)
    }
}
