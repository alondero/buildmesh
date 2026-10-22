package dev.buildmesh.remote

import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/**
 * The canonical mobile-route bodies, serialised from the Rust wire types and
 * committed under `src/test/resources/mobile-wire/` (issue #2318).
 *
 * The Android client reads untyped JSON, so its tests used to mock the
 * desktop with bodies written by hand. When the desktop changed a response
 * shape, those hand-written bodies kept passing. Reading the fixture the
 * Rust wire type produced instead means a wire change fails a test here too:
 * the committed fixture stops matching the Rust type, the Rust gate reports
 * the drift, and the regenerated fixture fails the client that has not caught
 * up.
 *
 * This is the instrumentation twin of the JVM loader in `src/test`: the same
 * files, read from the test APK's assets rather than the JVM classpath. The
 * Gradle build points the `androidTest` assets at `src/test/resources`, so
 * both suites read one committed copy.
 */
object MobileWireFixtures {
    /** The body of a mobile route, exactly as the desktop sends it. */
    fun body(name: String): JsonElement = Json.parseToJsonElement(raw(name))

    /** The body of a mobile route, when it is a JSON object. */
    fun obj(name: String): JsonObject = body(name) as? JsonObject
        ?: error("The mobile wire fixture $name.json is not a JSON object")

    /** The body of a mobile route, when it is a JSON array. */
    fun array(name: String): JsonArray = body(name) as? JsonArray
        ?: error("The mobile wire fixture $name.json is not a JSON array")

    /** The raw bytes of a mobile route, so a test can hand them to a mock. */
    fun raw(name: String): String = InstrumentationRegistry.getInstrumentation().context.assets
        .open("mobile-wire/$name.json")
        .bufferedReader()
        .use { it.readText() }
}
