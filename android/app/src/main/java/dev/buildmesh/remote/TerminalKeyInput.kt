package dev.buildmesh.remote

import android.annotation.SuppressLint
import android.content.Context
import android.text.InputType
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.webkit.WebView
import android.widget.FrameLayout

/**
 * Typing straight into the agent's own input box (issue #2747).
 *
 * The phone used to collect a whole line in its own text field and send it on
 * **Send**, so the agent never saw a keystroke until the line was finished:
 * its slash-command menu, `@` file completion and Tab completion could not
 * run. Each key now reaches the agent as it is typed.
 *
 * Input stays owned by Kotlin, not by the xterm in the WebView. That xterm is
 * display-only (`disableStdin`) because the desktop's own xterm already
 * answers the terminal queries in the agent's output; a second answering
 * terminal would type stray replies into the agent's prompt.
 */

/** Modes the agent switched on in its own output, as the phone's xterm tracks them. */
data class TerminalInputModes(
    /** DECCKM: arrows and Home/End send `ESC O x` instead of `ESC [ x`. */
    val applicationCursorKeys: Boolean = false,
    /** Mode 2004: the agent reads a pasted newline as text, not as Enter. */
    val bracketedPaste: Boolean = false,
)

private const val ESC = "\u001b"
private const val DEL = "\u007f"

/**
 * The bytes a desktop terminal sends for one key, or `null` for a key the
 * terminal does not own (Back, volume, a bare modifier), so the system keeps
 * handling it. Navigation keys with modifiers use xterm's `1;<modifier>`
 * parameter, which is what agent TUIs parse for Ctrl+arrow word movement.
 */
fun terminalKeyBytes(
    keyCode: Int,
    unicodeChar: Int,
    shift: Boolean = false,
    ctrl: Boolean = false,
    alt: Boolean = false,
    modes: TerminalInputModes = TerminalInputModes(),
): String? {
    val modifier = 1 + (if (shift) 1 else 0) + (if (alt) 2 else 0) + (if (ctrl) 4 else 0)
    val meta = if (alt) ESC else ""
    fun cursor(final: Char) = when {
        modifier > 1 -> "$ESC[1;$modifier$final"
        modes.applicationCursorKeys -> "${ESC}O$final"
        else -> "$ESC[$final"
    }
    fun tilde(number: Int) = if (modifier > 1) "$ESC[$number;$modifier~" else "$ESC[$number~"
    return when (keyCode) {
        KeyEvent.KEYCODE_DPAD_UP -> cursor('A')
        KeyEvent.KEYCODE_DPAD_DOWN -> cursor('B')
        KeyEvent.KEYCODE_DPAD_RIGHT -> cursor('C')
        KeyEvent.KEYCODE_DPAD_LEFT -> cursor('D')
        KeyEvent.KEYCODE_MOVE_HOME -> cursor('H')
        KeyEvent.KEYCODE_MOVE_END -> cursor('F')
        KeyEvent.KEYCODE_INSERT -> tilde(2)
        KeyEvent.KEYCODE_FORWARD_DEL -> tilde(3)
        KeyEvent.KEYCODE_PAGE_UP -> tilde(5)
        KeyEvent.KEYCODE_PAGE_DOWN -> tilde(6)
        KeyEvent.KEYCODE_DEL -> meta + if (ctrl) "\b" else DEL
        KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> "$meta\r"
        KeyEvent.KEYCODE_TAB -> if (shift) "$ESC[Z" else "$meta\t"
        KeyEvent.KEYCODE_ESCAPE -> ESC
        else -> printable(unicodeChar, ctrl)?.let { meta + it }
    }
}

private fun printable(char: Int, ctrl: Boolean): String? {
    if (char <= 0) return null
    val text = String(Character.toChars(char))
    if (!ctrl) return text
    val control = when (char.toChar()) {
        in 'a'..'z' -> char - 'a'.code + 1
        in 'A'..'Z' -> char - 'A'.code + 1
        ' ', '@', '2' -> 0
        '[', '3' -> 27
        '\\', '4' -> 28
        ']', '5' -> 29
        '^', '6' -> 30
        '_', '/', '7' -> 31
        '?', '8' -> 127
        else -> return text
    }
    return control.toChar().toString()
}

/** A pasted or typed newline is Enter to a terminal: xterm sends CR for both. */
private fun carriageReturns(text: String) = text.replace("\r\n", "\r").replace('\n', '\r')

/**
 * Text the Compose sheet puts into the agent's own input box. Newlines become
 * CR, as in an xterm paste; when the agent has bracketed paste on, the markers
 * keep those newlines inside the draft instead of submitting it line by line.
 * Submitting is a separate Enter — see [COMPOSE_SUBMIT_GAP_MS].
 */
fun composePaste(text: String, modes: TerminalInputModes): String {
    val body = carriageReturns(text)
    return if (modes.bracketedPaste) "$ESC[200~$body$ESC[201~" else body
}

/**
 * The gap between the Compose sheet's paste and its Enter. The desktop's
 * ordinary prompt delivery uses the same 1s gap: an Enter that lands in the
 * same burst as the paste can be read as part of it (Codex keeps it as a
 * newline), leaving the prompt unsent in the agent's box.
 */
const val COMPOSE_SUBMIT_GAP_MS = 1_000L

/**
 * Turns soft-keyboard edits into the keystrokes that reproduce them in the
 * agent's box.
 *
 * The keyboard is asked for suggestion-free input, so most keyboards commit
 * one character at a time. A keyboard that still holds back a word
 * ("composing") replaces the whole word on each key; streaming only the
 * difference — backspaces for what changed, then the new text — keeps the
 * agent's box equal to what the keyboard shows, letter by letter, so the
 * agent's menus still filter as the user types. Differences are counted in
 * code points: one backspace erases one character in the agent's box, and an
 * emoji is one character there though two UTF-16 units here.
 */
class ImeTextStream {
    private var composing = ""

    fun setComposing(text: CharSequence): String {
        val next = carriageReturns(text.toString())
        return replaceComposing(next).also { composing = next }
    }

    fun commit(text: CharSequence): String =
        replaceComposing(carriageReturns(text.toString())).also { composing = "" }

    /** The held-back word is already in the agent's box; only forget it. */
    fun finishComposing(): String {
        composing = ""
        return ""
    }

    fun deleteBefore(count: Int): String {
        if (count <= 0) return ""
        val kept = (composing.codePointCount(0, composing.length) - count).coerceAtLeast(0)
        composing = composing.substring(0, composing.offsetByCodePoints(0, kept))
        return DEL.repeat(count)
    }

    private fun replaceComposing(next: String): String {
        var shared = 0
        while (shared < composing.length && shared < next.length) {
            val point = composing.codePointAt(shared)
            if (point != next.codePointAt(shared)) break
            shared += Character.charCount(point)
        }
        val erase = composing.codePointCount(shared, composing.length)
        return DEL.repeat(erase) + next.substring(shared)
    }
}

/**
 * The terminal surface: the display-only WebView, wrapped by the view that
 * owns the soft keyboard.
 *
 * The wrapper, not the WebView, is the keyboard's target. Android 9 and later
 * refuse focus to a zero-sized view, so a hidden helper view cannot hold the
 * keyboard; the wrapper is full size and blocks focus from reaching the
 * WebView, so Chromium never opens a keyboard of its own for xterm's
 * textarea. Touches still go to the WebView, which keeps scrolling and link
 * taps; a plain tap calls back into [showKeyboard] over the JS bridge.
 */
@SuppressLint("ViewConstructor")
class TerminalSurface(
    context: Context,
    val terminal: WebView,
    private val modes: () -> TerminalInputModes,
    private val send: (String) -> Unit,
) : FrameLayout(context) {
    private val stream = ImeTextStream()
    private val keyboard = context.getSystemService(InputMethodManager::class.java)

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        descendantFocusability = FOCUS_BLOCK_DESCENDANTS
        terminal.isFocusable = false
        terminal.isFocusableInTouchMode = false
        contentDescription = "Agent terminal. Tap to type into the agent's input."
        addView(terminal, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
    }

    fun showKeyboard() {
        requestFocus()
        keyboard?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }

    fun hideKeyboard() {
        keyboard?.hideSoftInputFromWindow(windowToken, 0)
    }

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // Visible-password with no suggestions is the strongest request for
        // one-character-at-a-time input that keyboards honour; Termux makes
        // the same request when it needs character input. Personalised
        // learning is off because terminal input is not prose to learn from.
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_FLAG_NO_ENTER_ACTION or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING or
            EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this@TerminalSurface, true) {
            // The editable is cleared after every commit, so the keyboard
            // sees nothing before the cursor and sends Backspace as a key
            // event (which [onKeyDown] maps) instead of editing text it
            // believes is there.
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                emit(stream.commit(text))
                super.commitText(text, newCursorPosition)
                editable?.clear()
                return true
            }
            override fun setComposingText(text: CharSequence, newCursorPosition: Int): Boolean {
                emit(stream.setComposing(text))
                return super.setComposingText(text, newCursorPosition)
            }
            override fun finishComposingText(): Boolean {
                stream.finishComposing()
                super.finishComposingText()
                editable?.clear()
                return true
            }
            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                emit(stream.deleteBefore(beforeLength))
                return super.deleteSurroundingText(beforeLength, afterLength)
            }
            override fun performEditorAction(editorAction: Int): Boolean {
                emit("\r")
                return true
            }
        }
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        // Ctrl and Alt are applied by [terminalKeyBytes]; read the base
        // character without them, so Ctrl+C is `c` plus Ctrl rather than the
        // character Android would map the chord to.
        val base = event.getUnicodeChar(event.metaState and (KeyEvent.META_CTRL_MASK or KeyEvent.META_ALT_MASK).inv())
        if (base and KeyCharacterMap.COMBINING_ACCENT != 0) return true
        val bytes = terminalKeyBytes(keyCode, base, shift = event.isShiftPressed, ctrl = event.isCtrlPressed,
            alt = event.isAltPressed, modes = modes()) ?: return super.onKeyDown(keyCode, event)
        emit(bytes)
        return true
    }

    override fun onKeyMultiple(keyCode: Int, repeatCount: Int, event: KeyEvent): Boolean {
        val characters = event.characters
        if (keyCode != KeyEvent.KEYCODE_UNKNOWN || characters == null) return super.onKeyMultiple(keyCode, repeatCount, event)
        emit(stream.commit(characters))
        return true
    }

    private fun emit(bytes: String) {
        if (bytes.isNotEmpty()) send(bytes)
    }
}
