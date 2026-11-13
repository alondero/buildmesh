package dev.buildmesh.remote

import android.view.KeyEvent
import org.junit.Assert.*
import org.junit.Test

/**
 * Typing straight into the agent's own input box (issue #2747).
 *
 * The phone sends each key as it is typed, so the bytes must be the ones a
 * desktop terminal would send for the same key — otherwise the agent's own
 * menus, completion and line editing see a different key than the user
 * pressed.
 */
class TerminalKeyInputTest {
    private val normal = TerminalInputModes()
    private val appCursor = TerminalInputModes(applicationCursorKeys = true)

    private fun key(code: Int, modes: TerminalInputModes = normal, char: Int = 0,
                    shift: Boolean = false, ctrl: Boolean = false, alt: Boolean = false) =
        terminalKeyBytes(code, char, shift = shift, ctrl = ctrl, alt = alt, modes = modes)

    @Test fun editingKeysSendTheTerminalSequences() {
        assertEquals("\u007f", key(KeyEvent.KEYCODE_DEL))
        assertEquals("\u001b[3~", key(KeyEvent.KEYCODE_FORWARD_DEL))
        assertEquals("\r", key(KeyEvent.KEYCODE_ENTER))
        assertEquals("\r", key(KeyEvent.KEYCODE_NUMPAD_ENTER))
        assertEquals("\t", key(KeyEvent.KEYCODE_TAB))
        assertEquals("\u001b[Z", key(KeyEvent.KEYCODE_TAB, shift = true))
        assertEquals("\u001b", key(KeyEvent.KEYCODE_ESCAPE))
        assertEquals("\u001b[5~", key(KeyEvent.KEYCODE_PAGE_UP))
        assertEquals("\u001b[6~", key(KeyEvent.KEYCODE_PAGE_DOWN))
    }

    @Test fun arrowsFollowTheAgentsCursorKeyMode() {
        assertEquals("\u001b[A", key(KeyEvent.KEYCODE_DPAD_UP))
        assertEquals("\u001b[B", key(KeyEvent.KEYCODE_DPAD_DOWN))
        assertEquals("\u001b[C", key(KeyEvent.KEYCODE_DPAD_RIGHT))
        assertEquals("\u001b[D", key(KeyEvent.KEYCODE_DPAD_LEFT))
        assertEquals("\u001b[H", key(KeyEvent.KEYCODE_MOVE_HOME))
        assertEquals("\u001b[F", key(KeyEvent.KEYCODE_MOVE_END))
        assertEquals("\u001bOA", key(KeyEvent.KEYCODE_DPAD_UP, appCursor))
        assertEquals("\u001bOD", key(KeyEvent.KEYCODE_DPAD_LEFT, appCursor))
        assertEquals("\u001bOH", key(KeyEvent.KEYCODE_MOVE_HOME, appCursor))
    }

    @Test fun modifiedNavigationKeysCarryTheXtermModifierParameter() {
        assertEquals("\u001b[1;5D", key(KeyEvent.KEYCODE_DPAD_LEFT, ctrl = true))
        assertEquals("\u001b[1;2A", key(KeyEvent.KEYCODE_DPAD_UP, appCursor, shift = true))
        assertEquals("\u001b[1;3C", key(KeyEvent.KEYCODE_DPAD_RIGHT, alt = true))
        assertEquals("\u001b[3;5~", key(KeyEvent.KEYCODE_FORWARD_DEL, ctrl = true))
    }

    @Test fun ctrlAndAltChordsFromAHardwareKeyboard() {
        assertEquals("\u0003", key(KeyEvent.KEYCODE_C, char = 'c'.code, ctrl = true))
        assertEquals("\u0003", key(KeyEvent.KEYCODE_C, char = 'C'.code, ctrl = true, shift = true))
        assertEquals("\u001b", key(KeyEvent.KEYCODE_LEFT_BRACKET, char = '['.code, ctrl = true))
        assertEquals("\u0000", key(KeyEvent.KEYCODE_SPACE, char = ' '.code, ctrl = true))
        assertEquals("\u001bb", key(KeyEvent.KEYCODE_B, char = 'b'.code, alt = true))
        assertEquals("\u001b\u007f", key(KeyEvent.KEYCODE_DEL, alt = true))
        assertEquals("\u0008", key(KeyEvent.KEYCODE_DEL, ctrl = true))
        assertEquals("\u001b\r", key(KeyEvent.KEYCODE_ENTER, alt = true))
    }

    @Test fun printableKeysSendTheirCharacterAndUnknownKeysAreLeftToTheSystem() {
        assertEquals("a", key(KeyEvent.KEYCODE_A, char = 'a'.code))
        assertEquals("/", key(KeyEvent.KEYCODE_SLASH, char = '/'.code))
        assertNull("Back must reach the activity, not the agent", key(KeyEvent.KEYCODE_BACK))
        assertNull("Volume keys are the system's", key(KeyEvent.KEYCODE_VOLUME_UP))
        assertNull("A bare modifier sends nothing", key(KeyEvent.KEYCODE_SHIFT_LEFT))
    }

    @Test fun committedTextReachesTheAgentAsTypedWithEnterAsCarriageReturn() {
        val stream = ImeTextStream()
        assertEquals("/", stream.commit("/"))
        assertEquals("s", stream.commit("s"))
        assertEquals("\r", stream.commit("\n"))
        assertEquals("a\rb", stream.commit("a\r\nb"))
    }

    @Test fun aKeyboardThatHoldsBackAWordStillStreamsEachLetter() {
        // A keyboard that insists on composing replaces the whole held-back
        // word on each key. The agent's box only ever sees the difference, so
        // it stays equal to what the keyboard shows.
        val stream = ImeTextStream()
        assertEquals("/", stream.setComposing("/"))
        assertEquals("s", stream.setComposing("/s"))
        assertEquals("k", stream.setComposing("/sk"))
        // Autocorrect rewrites the word: erase only what differs ("k"), keep "/s".
        assertEquals("\u007fhip", stream.setComposing("/ship"))
        assertEquals("\u007f\u007f\u007fkill", stream.setComposing("/skill"))
        // Committing the same word sends nothing more; the next word starts clean.
        assertEquals("", stream.commit("/skill"))
        assertEquals(" ", stream.commit(" "))
        assertEquals("o", stream.setComposing("o"))
        assertEquals("", stream.finishComposing())
        assertEquals("k", stream.setComposing("k"))
    }

    @Test fun backspaceInsideAHeldBackWordErasesWholeCharacters() {
        val stream = ImeTextStream()
        assertEquals("✓😀", stream.setComposing("✓😀"))
        // One emoji is one character in the agent's box, though two UTF-16 units.
        assertEquals("\u007f", stream.setComposing("✓"))
        assertEquals("\u007f\u007f", stream.deleteBefore(2))
        assertEquals("", stream.setComposing(""))
    }

    @Test fun composeSendPastesThenSubmitsSeparately() {
        val bracketed = TerminalInputModes(bracketedPaste = true)
        assertEquals("fix the build", composePaste("fix the build", normal))
        assertEquals("line one\rline two", composePaste("line one\nline two", normal))
        assertEquals("\u001b[200~line one\rline two\u001b[201~", composePaste("line one\r\nline two", bracketed))
        assertEquals("\u001b[200~one line\u001b[201~", composePaste("one line", bracketed))
    }

    @Test fun quickKeysFollowTheSameModes() {
        val up = terminalQuickKeys.first { it.first == "↑" }.second
        assertEquals("\u001b[A", up(normal))
        assertEquals("\u001bOA", up(appCursor))
        assertEquals("\u0003", terminalQuickKeys.first { it.first == "Ctrl+C" }.second(appCursor))
    }
}
