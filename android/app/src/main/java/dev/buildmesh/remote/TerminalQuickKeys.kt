package dev.buildmesh.remote

import android.view.KeyEvent

/**
 * Keys under the Android terminal, and the one-shot modifiers that compose
 * the chords a phone keyboard cannot type.
 *
 * The row is the keys harness CLIs are actually driven by, not a list of
 * every chord. Esc stops or dismisses, Tab completes and queues, the arrows
 * move through menus and history, Enter confirms while the keyboard is
 * closed, and Ctrl+C stops the agent in one tap. Shift and Ctrl apply to the
 * next key and then drop, so Shift then Left is the modified arrow Codex uses
 * to answer a question, Shift then Tab cycles mode, and Ctrl then J is a
 * newline. A second tap of the same modifier cancels it. Bare y and n stay
 * off this row: the band above the terminal names the request it answers.
 */
val quickModifiers: List<QuickModifier> = listOf(
    QuickModifier("Shift", { it.shiftOn }, { it.toggleShift() }),
    QuickModifier("Ctrl", { it.ctrlOn }, { it.toggleCtrl() }),
)

val quickModifierLabels: List<String> = quickModifiers.map { it.label }

private val ARROWS = setOf(
    KeyEvent.KEYCODE_DPAD_UP,
    KeyEvent.KEYCODE_DPAD_DOWN,
    KeyEvent.KEYCODE_DPAD_LEFT,
    KeyEvent.KEYCODE_DPAD_RIGHT,
)

val terminalQuickKeys: List<TerminalQuickKey> = listOf(
    TerminalQuickKey("Esc", KeyEvent.KEYCODE_ESCAPE),
    TerminalQuickKey("Ctrl+C", KeyEvent.KEYCODE_C, char = 'c'.code, interrupt = true),
    TerminalQuickKey("Tab", KeyEvent.KEYCODE_TAB),
    TerminalQuickKey("↑", KeyEvent.KEYCODE_DPAD_UP),
    TerminalQuickKey("↓", KeyEvent.KEYCODE_DPAD_DOWN),
    TerminalQuickKey("←", KeyEvent.KEYCODE_DPAD_LEFT),
    TerminalQuickKey("→", KeyEvent.KEYCODE_DPAD_RIGHT),
    TerminalQuickKey("Enter", KeyEvent.KEYCODE_ENTER),
)

class QuickModifier(val label: String, val on: (QuickModifierLatch) -> Boolean, val toggle: (QuickModifierLatch) -> Unit)

class TerminalQuickKey(val label: String, val keyCode: Int, val char: Int = 0, val interrupt: Boolean = false) {
    /** Shift changes Tab and the arrows. It does not change Enter: Enter submits. */
    val takesShift: Boolean = keyCode == KeyEvent.KEYCODE_TAB || keyCode in ARROWS

    /** Ctrl changes the arrows (word movement). A control letter is typed, not a strip key. */
    val takesCtrl: Boolean = keyCode in ARROWS

    fun caption(shift: Boolean, ctrl: Boolean): String {
        if (interrupt) return label
        val shifted = shift && takesShift
        val controlled = ctrl && takesCtrl
        return when {
            shifted && controlled -> "⇧^$label"
            shifted && label == "Tab" -> "⇧Tab"
            shifted -> "⇧$label"
            controlled -> "^$label"
            else -> label
        }
    }
}

/**
 * Shift and Ctrl for the next key.
 *
 * [onChanged] runs on the thread that armed or consumed a modifier. The
 * terminal pane uses it to redraw the row, including when the keyboard
 * consumes Ctrl.
 */
class QuickModifierLatch(private val onChanged: () -> Unit = {}) {
    var shiftOn: Boolean = false
        private set
    var ctrlOn: Boolean = false
        private set

    fun toggleShift() = update { shiftOn = !shiftOn }
    fun toggleCtrl() = update { ctrlOn = !ctrlOn }

    fun effectiveShift(hardwareShift: Boolean) = hardwareShift || shiftOn
    fun effectiveCtrl(hardwareCtrl: Boolean) = hardwareCtrl || ctrlOn

    /** Bytes for one strip key. The latches then drop, including a latch the key ignores. */
    fun sequence(key: TerminalQuickKey, modes: TerminalInputModes): String {
        val shift = shiftOn && key.takesShift
        val ctrl = key.interrupt || (ctrlOn && key.takesCtrl)
        val bytes = checkNotNull(terminalKeyBytes(key.keyCode, key.char, shift = shift, ctrl = ctrl, modes = modes))
        consume()
        return bytes
    }

    /**
     * A committed character, with Ctrl applied to the first character after
     * any corrections. A delete-only or empty diff is not that character, so
     * the latch stays armed.
     */
    fun onCommit(diff: String): String {
        if (diff.isEmpty() || diff.all { it == '\u007f' }) return diff
        val out = applyCtrlToFirstCharacter(diff, ctrlOn)
        consume()
        return out
    }

    /** Enter from the soft keyboard. It submits, and it drops a modifier that was still armed. */
    fun onSubmit(): String {
        consume()
        return "\r"
    }

    fun consume() {
        if (!shiftOn && !ctrlOn) return
        shiftOn = false
        ctrlOn = false
        onChanged()
    }

    private fun update(change: () -> Unit) {
        change()
        onChanged()
    }
}
