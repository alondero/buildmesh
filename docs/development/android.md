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

The debug APK is suitable for local installation. The Android workflow produces a
debug APK, executed JVM test report and strict lint report on relevant changes.

## Release signing and distribution

Signed APKs are published on the GitHub Release beside the Windows installers,
as `Buildmesh_<version>_android.apk`. The `Release` workflow builds and signs
it with a developer-owned key held in repository secrets; **no key or keystore
is stored in this repository**, and the `release` build type is unsigned
whenever those secrets are absent.

Three repository secrets are required (Settings -> Secrets and variables ->
Actions):

| Secret | Contents |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | the `.jks`, base64-encoded (`base64 -w0 < release.jks`) |
| `ANDROID_KEY_ALIAS` | the key alias inside that keystore |
| `ANDROID_KEY_PASSWORD` | the keystore and key password |

Generate the keystore once and keep the file outside the repository:

```powershell
keytool -genkeypair -v -keystore buildmesh-release.jks -alias buildmesh \
  -keyalg RSA -keysize 2048 -validity 10000
```

Set each secret from a shell that stores the exact bytes, for example
`base64 -w0 < release.jks | gh secret set ANDROID_KEYSTORE_BASE64` in Git Bash
(piping avoids leaving a base64 copy of the key in the working tree). Do not
pipe a string from Windows PowerShell 5.1 (`$s | gh secret set ...`): it
appends `\r\n`. That broke the first v1.4.1 run, where the job failed with
`base64: invalid input` after the Windows release was already in draft. The job
now strips trailing whitespace from the keystore secret and trailing newlines
from the alias and password before using them, so a secret stored that way no
longer fails the job, but set them cleanly anyway. If a run does fail, re-set
the secrets and run `gh run rerun <run-id> --failed`; the run id is required in
a non-interactive shell.

Until those secrets exist the Android job emits a visible warning and the
release ships Windows-only; it does not fail the release, because by that
point the Windows build has already been published and failing would strand
it as a draft.

The APK takes its `versionName` and `versionCode` from `package.json` rather
than a literal in `build.gradle.kts`, so it cannot disagree with the installer
next to it: `versionCode` is `major * 10000 + minor * 100 + patch`, which
leaves room for 100 patch releases per minor. At a release tag the manifest
holds the bare `X.Y.Z`, so the APK reports exactly that.

Two deliberate choices in the `release` build type:

- **No R8 minification or resource shrinking.** `lintVitalRelease` runs, but
  minification stays off until keep rules are written and verified against the
  live device suite, which would otherwise silently strip the WebSocket and
  Compose paths this app depends on.
- **Not added to `latest.json`.** That feed is the Tauri updater and covers the
  Windows and macOS desktop clients; the Android client updates by sideloading
  a newer APK.

The APK is published with a sidecar `<name>.sha256`. `SHA256SUMS.txt` covers the
Windows installers, and the macOS legs publish their own
`SHA256SUMS-macos-<arch>.txt` manifests, because that file is written before this
job runs.

## Pairing and transport

Enable Remote Access on the desktop. In the Android app choose **Scan pairing
QR**, scan the desktop's Connect QR, and choose **Pair with desktop**. The same
QR still works in the browser. Its fragment carries `pair` and, for HTTPS,
`ca`, the SHA-256 fingerprint of the desktop root CA. Older desktops require
pasting the Root CA fingerprint into the app along with the full pairing URL.

Every QR the desktop renders in that modal — connect, Android install and iOS
install — must be dark-on-light. The scanner binarises on luminance through
ZXing's `HybridBinarizer`, which assumes dark modules on a light background and
has no inverted fallback. An inverted code throws `NotFoundException` on every
frame, so the camera preview runs and never decodes and the user sees no error,
only a scanner that never responds.

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
and device transfer exclude app data. Desktop Authorized Devices remains the
revocation authority. **Forget this desktop** clears this phone's credential;
it does not revoke the server record.

Which failures may clear that credential is narrow, because the desktop's two
auth statuses mean different things (issue #2317). A 401 is the desktop's
verdict on the credential — `auth::authorize` answered because no valid device
token was presented — so a 401 from a data route, or a 401/403 from the
`/api/session` restore that re-presents it, returns the phone to pairing. A 403
is a request the desktop refused *after* the device authenticated (a PR create
naming a node from another mesh, a route outside the credential's scope), so it
becomes an ordinary error message and the stored session survives. A network
failure is not a verdict on the credential and retains it for retry.

There is deliberately no recovery refresh here. The browser client re-mints its
cookie from a separate `localStorage` token because the browser hides that
HttpOnly cookie from JavaScript
([mobile.md](mobile.md#session-cookie-recovery)); the cookie stored on the phone
*is* the device token, and `/api/session` mints that same token back, so
re-minting hands the device the credential it already presented and does not
recover a rejected one. The sockets follow the same split: a refused WebSocket
upgrade (403 for a ticket minted for another target, 401 for one that expired or
was already used) reconnects with a fresh ticket rather than revoking the
device, and `/api/ws-ticket`, which authorizes before it mints, is where a
revoked device does say so.

Cleartext is permitted only for loopback development and the Android emulator's
`10.0.2.2` host alias. Remote HTTP invitations, embedded URL credentials,
query-string invitations and redirects are rejected. Deep links use
`buildmesh://pair?url=<encoded-full-invitation>` and only populate the pairing
form; they do not authorize or pair automatically. A deep link is consumed once
— `MainActivity` clears the intent it arrived on — so the pairing form keeps
the URL in its own saved state, which is what survives a rotation or a process
recreation.

## Screens and ownership

The native UI uses the Desktop dark palette from `src/App.css`, mapped across
all Material colour roles in `Theme.kt`. Selected controls use cyan on the
selection surface; panels, menus and navigation retain the neutral surface
steps. Geist is bundled in the APK, including its SIL Open Font License, so
typography and the wordmark work offline. Controls keep Android touch targets
with 8dp corners, 10dp cards and fields, and 12dp dialogs.

Pairing and the home app bar show the full Relay badge beside the lowercase,
two-colour `buildmesh` wordmark. Detail screens retain their contextual titles.
The adaptive launcher icon uses the compact Relay mark, with a transparent
monochrome silhouette for Android's themed icons. The vector resources
are generated from `docs/brand/` by `npm run brand:build -- --android-only`;
edit the canonical SVGs rather than native copies. The native client currently
uses the dark theme regardless of the phone's system appearance.
`MainActivity` owns the transparent system bars and their dark appearance through
its edge-to-edge setup; the resource theme supplies the initial window background.

Compose owns Overview/Work, mesh selection and search, node details and replies,
task capture, issue spawning, archive import/resume, changed files, selectable
diffs, mesh-branch PR creation and connection settings. Connection settings also
owns the background notifications switch, which is off until the user turns it
on; the watch it starts is described under
[Background agent notices](#background-agent-notices). Launch selections come
from the backend menu, including saved configurations and capability filtering.
Tapping an Overview mesh name selects its ID and switches to Work, clearing the
search and attention filter. Mesh headers summarize open (non-archived) nodes
with wrapping status dots and a count, independent of dashboard filters. Empty
mesh names and paths use the muted text token; their actions remain available.
Work uses compact rows under mesh headers, with the desktop status-circle shapes
and Circuit orbit glyphs. Circuit-linked agents share one bracket, ordered with
the representative first; each row opens that agent. Search and Needs attention
match individual agents and retain the matching activity's other members for
context. Missing or archived parents, cross-mesh links and cycles leave agents
accessible. Desktop manual groups remain local desktop view state.
`GET /api/circuit-agent-ownerships` supplies the same ownership ledger as the
desktop sidebar, using the generated `CircuitAgentOwnership` wire type. Older
desktops without this route show standalone rows. A failed ownership refresh
retains the previous relationships while node updates continue.
Drafts persist privately on the phone, scoped to the verified desktop endpoint
and trusted certificate, until an acknowledged action succeeds. Re-pairing the
same desktop restores its drafts without exposing them to another desktop.
A saved Launch Configuration row's `id` is the configuration's id, not a Spawn
Option, so Capture, issue and pull request launches send it as `configuration_id` beside the row's own
`spawn_option_id` (the same pair the web client sends). **Mesh default** sends
neither, and the desktop resolves its own default chain.

Mesh cards, issue cards and the mesh's open pull requests carry the same split
button as the desktop spawn cluster (`SpawnSplitButton`). The main half names the
mesh's pinned default, or reads **Mesh default** when the app-wide default
applies, which the phone cannot see. The chevron lists the backend menu in its
own order, saved configurations included, without unavailable rows. On a mesh
card both halves open Capture with that mesh and launch already selected, because
Capture is where the first prompt is written; an empty prompt opens a plain
session. Capture keeps an explicit choice that cannot take a prompt and explains
why, instead of silently switching to another harness. On an issue or pull
request the menu offers only prompt-capable rows, and a tap starts the agent at
once, as on the desktop. `POST /api/meshes/{id}/issues/{n}/spawn` and
`POST /api/meshes/{id}/pulls/{n}/spawn` run the desktop probes'
`create_issue_node` and `create_pr_node`. The node row is returned straight
away, and its status reports the background worktree and process start. The
backend renders the issue or review prompt from the user's template, so the
phone sends only a title hint and, for a pull request, the head fields from the
feed. Archive resume accepts a Spawn Option only, so its picker offers no saved
configurations and requires a resumable option.
Archive HTTP 207 dismisses the launch dialog and reports the imported node and
launch failure, so a user can inspect Work before attempting another import.
Pull requests are created from an Agent Node, because the create route requires
`node_id` (`create_requires_a_node_id_in_the_body`): the node screen previews `GET /api/meshes/{id}/pr/source?node_id=` and submits
`node_id` with the previewed `head_branch`, so a worktree that moved since the
preview is rejected instead of published (see the
[mobile client](mobile.md#create-pr-is-node-scoped)).

Node details also show the same-repository open PR for the live worktree
branch, with checks, review decision and mergeability. **Squash and merge**
requires confirmation, rechecks mergeability, and deletes the branch after
merging. The mesh's **Pull requests** shortcut lists open PRs and discloses
truncated results. Status refreshes while foregrounded; **Refresh PR status**
retries a failed read. See [PR status and squash merge](mobile.md#pr-status-and-squash-merge)
for gates and recovery after an interrupted merge.

The client reads untyped JSON, so the resource screens (issues, archive,
changes, diff, PR source) decode a response inside their loader's error
handling, and the JSON helpers raise a readable `IllegalStateException` rather
than a `ClassCastException`. A shape this build does not understand becomes an
"unexpected response" notice with Retry
(`anIssuesResponseThisBuildCannotReadIsShownAsAnErrorInsteadOfCrashing`); a
cast that throws while composing would instead terminate the app. The issues route returns the
`{items, completeness}` feed; the client also accepts the bare array older
desktops returned.

The Issues screen preserves completeness and displays partial-feed warnings,
filters by title/number/tag, and expands body previews on tap. Its tag editor
uses the same acknowledged single-label route and Autopilot context as the
[mobile web client](mobile.md#phone-issue-triage).

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

The ViewModel owns foreground polling and its share of the event feed. The poll reads one
snapshot (`/api/meshes`, `/api/nodes`, `/api/providers`) every 5s while
foregrounded, so it hits the desktop's provider-menu route even when nothing
changed; that route memoizes its derivation per preferences generation rather
than re-deriving per poll (see
[providers.md](providers.md#the-spawn-menu-derivation-is-memoized-per-preferences-generation)).
Refresh requests are coalesced and versioned so polling cannot starve slow
responses and older responses do not overwrite a newer event or session
(`RefreshController.refresh()` publishes only when the request's revision is
still current). The
event feed is process-wide, so the socket has one reconnect job and one owner
however many surfaces are watching it. Forgetting or revocation cancels
pending actions before their callbacks can clear drafts or navigate.
Backgrounding cancels polling; foregrounding immediately reconnects with
a new target-bound WebSocket ticket. Ticket rate limits allow one bounded retry.
Mutating HTTP requests and redirects are never automatically replayed.
HTTP requests explicitly close their connection to match the embedded server's
one-request transport; terminal WebSockets retain their separate live connection.

## Background agent notices

An agent can stop and wait: it asks a question, or it asks for permission to
run a command. The desktop records that state on the node (`awaiting_input`)
and publishes it on `/ws/events`. The phone can be told about it while the app
is in the background, through a foreground service the user turns on
themselves.

**The switch.** Connection settings has a "Background notifications" toggle.
It is off until the user turns it on. Turning it on asks for the notification
permission on Android 13 and later; if the permission is refused the switch
stays off and says why. Turning it off stops the service and clears the
notices it posted.

**What the phone does.** The service (`AgentWatchService`) holds the desktop's
event feed open. When a node's status becomes `awaiting_input`, the phone
posts one notice for that node, naming it and saying whether it needs an
answer or needs permission, and the notice opens that node. The notice is
cleared as soon as the node's state is resolved — including when the answer is
given on the desktop, which is the case that matters: the user is not
required to answer on the phone that was told.

**The watch follows the desktop.** A notice lives as long as the watch does and
the node still needs a person, so forgetting the desktop stops the watch and
takes its notices with it — the watch had a desktop to listen to, and now it
does not. Its own "Watching for agents" notification goes with it, so the
phone does not keep claiming to listen to a desktop it is not paired with. The
user's preference survives, so pairing again starts the watch again.

**One feed, one socket.** The dashboard and the watch service read the same
feed (`EventFeed`), which owns one WebSocket and one reconnect timer for the
whole process. Two sockets would double the desktop's subscriber count, the
phone's idle traffic and the ticket mint rate. The feed delivers on its own
worker threads, so each owner takes its callbacks on its own thread: the
dashboard on its main thread, where the refresher is documented to live, and
the watch service on a worker, where it reads the desktop.

**The delivery bound.** Under one second from the desktop's lifecycle event to
the notice on the phone while the socket is connected: the event crosses the
LAN or VPN on the open socket, and the first notice for a node the phone has
not seen also reads that node's name from `/api/nodes`. If the socket was
down, the notice arrives after the reconnect instead — the socket backs off
1, 2, 4, 8 and 16 seconds, so the bound becomes 16 seconds after the last
failed attempt.

**The cost.** The watch replaces the foreground dashboard's five-second
snapshot poll rather than adding to it. Its own idle traffic is one WebSocket
keepalive ping per 20-second interval (`pingInterval(20, TimeUnit.SECONDS)` in
`Connection.kt`): about 4,320 pings a day at that interval, each a single
small control frame, so the day's traffic is protocol overhead rather than
payload, and the desktop sends no frames of its own while the nodes are idle.
The phone also shows a persistent "Watching for agents" notification while the
watch is on, because a foreground service has to say what it is doing. The
on-device battery figure still needs the physical-device acceptance run; it is
not claimed here.

**What it is not.** The notice states the desktop's state, not a promise that
the answer has to be given on this phone. The Circuit "Needs approval" gate is
a separate person-needed state and is not covered here. A revoked device stops
the feed rather than reconnecting forever; pairing again is the only way back.

The decision is recorded in
[ADR 0045](../adr/0045-android-background-agent-notices.md).

## Verification

JVM tests exercise actual OkHttp requests against MockWebServer: pairing and
cookie restoration, revocation, a refused request that must not read as one,
a refused socket upgrade that mints a fresh ticket, redirect rejection,
target-bound tickets and
bounded rate-limit retry, literal input payloads, partial resume, certificate
pins and hostname verification, invitation validation and Unicode reply limits.
They are client transport evidence, not proof of the Rust server.
Controlled coroutine tests also cover slow polling, event invalidation and
late cleanup after a background/foreground transition.
The background watch adds two JVM suites: one that reads the feed's event
vocabulary (which lifecycle kind means a node needs a person, how a node row
reaches the same conclusion, and that each node owns its own notification id),
and one that drives the feed against MockWebServer to prove two owners share a
single socket, that the socket closes with its last owner, and that a revoked
device stops the feed instead of minting tickets forever.

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
Session lifecycle tests read the encrypted store back in each case: a 403 from a
data route keeps it, a 401 from a data route and a 401 or 403 from the restore
clear it, and an unreachable desktop keeps it.
These assert literal payloads, retained drafts after failures, and acknowledged
success. They do not create a real GitHub pull request. Their mocked responses
and the JVM `WireContractTest` fixtures follow the generated Rust wire types in
`src/types/generated/`. A fixture written in the client's own assumed shape keeps
passing after the desktop changes: the issues-feed change crashed the Issues
screen while the old bare-array fixture still passed.

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
