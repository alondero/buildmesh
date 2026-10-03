# Native Android client

Status: current. The `android/` Gradle project is a Kotlin and Jetpack Compose
remote client for the existing Buildmesh mobile HTTP API. Android 8.0 (API 26)
and later are supported; the build targets Android 16 (API 36).

## Build and install

Install JDK 17 or newer, Node/npm, and the Android SDK with platform 36 and
build tools 35.0.0. Run `npm ci` at the repository root. Set `ANDROID_HOME`, or
set `sdk.dir` in ignored `android/local.properties`. Android Studio can open
`android/` directly. The committed Gradle wrapper selects Gradle 8.12.

From the repository root on Windows:

```powershell
android\gradlew.bat -p android assembleDebug testDebugUnitTest lintDebug
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

On macOS/Linux, use `bash android/gradlew -p android` with the same tasks.
The pre-build task bundles the terminal with the repository's existing xterm
dependencies; it requires npm dependencies to be installed. Generated assets,
Gradle caches, APKs and machine-specific SDK paths are ignored.

The debug APK is suitable for local installation. For distribution, build
`assembleRelease` and sign the APK with a developer-owned Android signing key;
keys are never stored in this repository. The Android workflow produces a
debug APK, executed JVM test report and strict lint report on relevant changes.

## Pairing and transport

Enable Remote Access on the desktop. In the Android app choose **Scan pairing
QR**, scan the desktop's Connect QR, and choose **Pair with desktop**. The same
QR still works in the browser. Its fragment carries `pair` and, for HTTPS,
`ca`, the SHA-256 fingerprint of the desktop root CA. Older desktops require
pasting the Root CA fingerprint into the app along with the full pairing URL.

An isolated bootstrap client downloads only `/install-cert.der`, without
credentials, cookies or redirects. It checks certificate validity and hostname
and verifies the downloaded root against the fingerprint obtained from the
desktop QR. The authenticated client then uses a normal X.509 trust manager
anchored solely at that root, with normal hostname verification. Invitations
are never sent before that trust check. Leaf renewal under the same root works;
root rotation requires fresh pairing. No system CA installation is needed.

`POST /api/pair` exchanges the invitation for the existing HttpOnly
`bm_session` cookie. The app stores origin, CA and cookie in an atomic,
AES-GCM-encrypted private file; its key lives in Android Keystore. Cloud backup
and device transfer exclude app data. A restored client calls `/api/session`;
401/403 returns to pairing, while network failure retains the session for retry.
Desktop Authorized Devices remains the revocation authority. **Forget this
desktop** clears this phone's credential; it does not revoke the server record.

Cleartext is permitted only for loopback development and the Android emulator's
`10.0.2.2` host alias. Remote HTTP invitations, embedded URL credentials,
query-string invitations and redirects are rejected. Deep links use
`buildmesh://pair?url=<encoded-full-invitation>` and only populate the pairing
form; they do not authorize or pair automatically.

## Screens and ownership

Compose owns Overview/Work, mesh selection and search, node details and replies,
task capture, issue spawning, archive import/resume, changed files, selectable
diffs, mesh-branch PR creation and connection settings. Launch selections come
from the backend menu, including saved configurations and capability filtering.
Drafts persist privately on the phone, scoped to the verified desktop endpoint
and trusted certificate, until an acknowledged action succeeds. Re-pairing the
same desktop restores its drafts without exposing them to another desktop.
Issue launch requires a configuration that supports prefill; archive resume
requires a resumable configuration. Unavailable configurations are excluded.
Archive HTTP 207 dismisses the launch dialog and reports the imported node and
launch failure, so a user can inspect Work before attempting another import.
PR creation uses the mesh branch, matching the current web route; an agent
worktree PR can be requested through that agent's terminal.

Only the terminal is a WebView: it loads bundled xterm assets from
`WebViewAssetLoader`, blocks all other requests/navigation, and exposes
resize and ready callbacks. No remote page, cookie or credential enters that
WebView. Kotlin owns HTTP and WebSockets. The server's initial terminal snapshot
replaces local terminal state on reconnect; binary output retains UTF-8 decoder
state inside xterm. Native composer and control buttons own input; the xterm
display disables stdin so replayed terminal queries cannot inject synthetic
responses into the current desktop prompt. Input controls report disconnection instead of silently
queueing keystrokes. HTTP replies enforce the server's 1024-byte encoded-body
limit. Enumerated answers are shown as guidance rather than guessed PTY keys.

The ViewModel owns foreground polling and the event socket. Refresh requests
are coalesced and versioned so polling cannot starve slow responses and older
responses cannot overwrite a newer event or session. Each socket has one
reconnect job and one owner. Forgetting or revocation cancels pending actions
before their callbacks can clear drafts or navigate. Backgrounding
cancels sockets/polling; foregrounding immediately reconnects with a new
target-bound WebSocket ticket. Ticket rate limits allow one bounded retry.
Mutating HTTP requests and redirects are never automatically replayed.
HTTP requests explicitly close their connection to match the embedded server's
one-request transport; terminal WebSockets retain their separate live connection.

## Verification

JVM tests exercise actual OkHttp requests against MockWebServer: pairing and
cookie restoration, revocation, redirect rejection, target-bound tickets and
bounded rate-limit retry, literal input payloads, partial resume, certificate
pins and hostname verification, invitation validation and Unicode reply limits.
They are client transport evidence, not proof of the Rust server.
Controlled coroutine tests also cover slow polling, event invalidation and
late cleanup after a background/foreground transition.

```powershell
android\gradlew.bat -p android connectedDebugAndroidTest
```

On a connected device/emulator the instrumentation suite checks native pairing
controls, dashboard filtering/navigation and Android Keystore persistence and
forgetting, Main-thread network discipline, cancelled mutation callbacks,
snapshot ownership, diff retry and the real bundled WebView's snapshot/binary
rendering, visible pixels in the native window, viewport geometry, resize and
input, including replayed queries that must not become keystrokes.
Native form tests also exercise capture, replies, issue launch, archive partial
failure and PR authentication through real OkHttp requests to MockWebServer.
These assert literal payloads, retained drafts after failures, and acknowledged
success. They do not create a real GitHub pull request.

For actual desktop acceptance, start the dev profile with Remote Access enabled,
connect an Android device through adb, and run from the repository root:

```powershell
node scripts/check-android-live.mjs --device YOUR_ADB_SERIAL --origin https://YOUR_LAN_IP:2992
```

The optional `--bridge` and `--http` origins default to the dev profile's
loopback ports 2991 and 2992; they must be loopback HTTP origins. `--origin`
must be the desktop HTTPS address reachable directly from the phone. This test
does not use an HTTP fallback or TLS relay. Avoid concurrent device pairing
while it runs, so the new QA device can be identified unambiguously.

The runner builds an isolated `live` variant (`dev.buildmesh.remote.live`),
creates a disposable Git repository and mesh, and supplies a single-use
invitation through stdin to private app storage. It refuses to overwrite an
existing live QA installation. The headless `LiveDesktopTest` requires that
fixture and fails when it is missing; it is excluded from the default debug
instrumentation source set. No unlocked screen is required for this test.

The live test verifies fingerprint-based pairing, encrypted session restore,
20 batches of three concurrent HTTPS reads, native task creation, ViewModel
pause/resume, production terminal WebSocket input/output/resize/reconnect,
real Git status/diff, and revocation clearing the phone's credential. It checks
command output in the desktop PTY; visible terminal rendering is covered by the
separate default device suite. The runner revokes its controller and any
confirmed QA device, stops its fixture nodes, deletes its own mesh and uninstalls
both QA packages. Cleanup failures are reported as failures. Logs and the
disposable Git fixture remain in ignored `.tmp/android-live/`; credentials are
never written to host files or command arguments.

`scripts/check-android.mjs` builds both debug and live instrumentation and
requires executed, passing JVM tests. CI compiles the live suite; running it
still requires the reachable desktop and selected device above.

Build prerequisites follow the [Android Gradle Plugin 8.10 compatibility
table](https://developer.android.com/build/releases/agp-8-10-0-release-notes).
The Compose compiler uses the [Kotlin Compose compiler
plugin](https://developer.android.com/develop/ui/compose/setup-compose-dependencies-and-compiler).
