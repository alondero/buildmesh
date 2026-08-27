#requires -Version 5.1
<#
.SYNOPSIS
  One command for a trustworthy green bar on Windows / inside a git worktree.

.DESCRIPTION
  Raw `cargo test` / `vitest` silently misbehave in this repo's Windows worktrees.
  This wrapper bakes in the workarounds that are otherwise tribal knowledge (and
  that weaker models repeatedly forget), so the result you see is the truth:

    * Builds dist/mobile/ first — a fresh worktree has no dist/ (it's gitignored),
      and the Rust test `mobile_assets_include_built_index_html` rust-embeds
      dist/mobile/index.html, so `cargo test` panics without it.
    * Clears BUILDMESH_PREFILL — leaked from the agent env, it fails the unrelated
      `prefill_stays_argv_for_wsl` test.
    * Runs vitest with --pool=threads — the default forks pool silently times out
      every worker in a worktree and reports PASS(0) FAIL(0) exit 0 (false green).
    * Pins cargo to src-tauri/Cargo.toml — Bash-tool CWD doesn't persist reliably.

  Situational escalations NOT applied by default (add the flag if you hit them):
    -CleanRust         cargo clean -p buildmesh first (incremental stale binary:
                       new #[test] fns don't run, count looks unchanged)
    -SerialRust        cargo test -- --test-threads=1 (OnceCell/static Mutex tests
                       self-deadlock or interfere when run in parallel)

.PARAMETER Target
  unit | integration | rust | all | all-ts  (default: all)

  all    = mobile build + unit vitest + integration vitest + cargo test
           (the default green bar; integration added in issue #1257 to
           match the GitHub Actions quality job)
  all-ts = unit vitest + integration vitest + npm run build (full TS
           green bar, mirrors the TS gates the GitHub Actions quality job
           runs on PRs — local parity with CI without spending 13
           minutes on a Rust build)

.EXAMPLE
  scripts\check.ps1                 # full green bar (mobile build + unit + integration + rust)
  scripts\check.ps1 unit            # just the TS unit suite, correct pool
  scripts\check.ps1 rust -SerialRust
  scripts\check.ps1 all-ts          # full TS gates (unit + integration + build)
#>
[CmdletBinding()]
param(
  [ValidateSet('unit', 'integration', 'rust', 'all', 'all-ts')]
  [string]$Target = 'all',
  [switch]$CleanRust,
  [switch]$SerialRust
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot   # scripts/ -> repo root
$failed = @()

# A non-Git-for-Windows git.exe earlier in PATH (seen: devkitPro's MSYS2 git
# shadowing it in Buildmesh agent sessions) writes POSIX-style worktree gitdir
# paths ("/home/<user>/AppData/...") that Windows libgit2 can't resolve — ~20
# git::worktree / agent::spawn / commands::pr tests fail with
# "failed to resolve path '/home/<user>/...'". Pin Git for Windows first.
$gitForWindows = 'C:\Program Files\Git\cmd'
$gitOnPath = (Get-Command git -ErrorAction SilentlyContinue).Source
if ((Test-Path (Join-Path $gitForWindows 'git.exe')) -and ($gitOnPath -notlike "$gitForWindows*")) {
  Write-Host "== PATH git is '$gitOnPath' -> pinning $gitForWindows first ==" -ForegroundColor Yellow
  $env:PATH = "$gitForWindows;$env:PATH"
}

# Same trap, different binary: the http::tls cert tests shell out to `openssl`,
# and devkitPro's MSYS2 copy (seen shadowing it in agent PowerShell sessions,
# independently of which git.exe resolves) dies with "add_item ... failed"
# before doing any work — false-failing 4 tests. Git for Windows ships a
# working openssl in usr\bin; pin it first when openssl resolves elsewhere.
$gitUsrBin = 'C:\Program Files\Git\usr\bin'
$openSslOnPath = (Get-Command openssl -ErrorAction SilentlyContinue).Source
if ((Test-Path (Join-Path $gitUsrBin 'openssl.exe')) -and ($openSslOnPath -notlike 'C:\Program Files\Git\*')) {
  Write-Host "== PATH openssl is '$openSslOnPath' -> pinning $gitUsrBin first ==" -ForegroundColor Yellow
  $env:PATH = "$gitUsrBin;$env:PATH"
}

function Ensure-MobileBuilt {
  # Rebuild when index.html is missing OR empty — an interrupted prior build (Ctrl-C /
  # Defender lock) can leave a zero-byte/truncated index.html that a Test-Path-only gate
  # would wrongly accept, embedding a stale bundle (false green). This only guarantees a
  # *present, non-empty* index.html (what the rust test needs); if you changed mobile
  # source and want a guaranteed-fresh bundle, run `npm run build:mobile` (or delete
  # dist/mobile) first — this gate does not detect stale-but-complete assets.
  $indexHtml = Join-Path $repo 'dist\mobile\index.html'
  $present = (Test-Path $indexHtml) -and ((Get-Item $indexHtml).Length -gt 0)
  if (-not $present) {
    Write-Host '== dist/mobile missing or empty -> npm run build:mobile ==' -ForegroundColor Cyan
    Push-Location $repo
    try { & npm run build:mobile } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw 'build:mobile failed' }
  }
}

function Invoke-Unit {
  Write-Host '== unit (vitest --pool=threads) ==' -ForegroundColor Cyan
  Push-Location $repo
  # PowerShell 5.1 promotes any stderr line from a native exe (vitest's child
  # node) into a NativeCommandError record. Under the script-wide
  # $ErrorActionPreference = 'Stop' that becomes a *terminating* error and
  # aborts the `& npx vitest ...` call before vitest can even run its suite.
  # jsdom's "HTMLCanvasElement.getContext() without canvas npm package"
  # warning is the trigger we keep hitting on Windows.
  # Fix: locally downgrade to 'Continue' so the warning passes through as
  # text, then trust $LASTEXITCODE for the pass/fail signal (real vitest
  # failures exit non-zero and still surface as a unit failure).
  $prevPref = $ErrorActionPreference
  try {
    $ErrorActionPreference = 'Continue'
    & npx vitest run --pool=threads tests/unit
  } finally {
    $ErrorActionPreference = $prevPref
    Pop-Location
  }
  if ($LASTEXITCODE -ne 0) { $script:failed += 'unit' }
}

function Invoke-Integration {
  # Issue #1257 — the jsdom integration suite (terminal container
  # reuse/reparenting, focus guardian, auto-spawn) ships and is
  # maintained, but used to only run when a developer remembered.
  # CI now gates it; this gives the same gate locally. No Tauri runtime
  # required — same `--pool=threads` rationale as Invoke-Unit.
  Write-Host '== integration (vitest --pool=threads) ==' -ForegroundColor Cyan
  Push-Location $repo
  $prevPref = $ErrorActionPreference
  try {
    $ErrorActionPreference = 'Continue'
    & npx vitest run --pool=threads tests/integration
  } finally {
    $ErrorActionPreference = $prevPref
    Pop-Location
  }
  if ($LASTEXITCODE -ne 0) { $script:failed += 'integration' }
}

function Invoke-TsBuild {
  # Mirrors the TS-side gating the GitHub Actions quality job runs.
  # `npm run build` is "tsc && vite build && vite build --mode mobile"
  # (see package.json:8) so it transitively runs tsc already — calling
  # `npx tsc --noEmit` beforehand would run the compiler twice and add
  # no extra signal. The build step is also what surfaces drift against
  # the generated bindings (issue #359) the same way CI does.
  Write-Host '== npm run build (tsc + vite + mobile) ==' -ForegroundColor Cyan
  Push-Location $repo
  $prevPref = $ErrorActionPreference
  try {
    $ErrorActionPreference = 'Continue'
    & npm run build
  } finally {
    $ErrorActionPreference = $prevPref
    Pop-Location
  }
  if ($LASTEXITCODE -ne 0) { $script:failed += 'build' }
}

function Invoke-Rust {
  Write-Host '== rust (cargo test) ==' -ForegroundColor Cyan
  # Clear the leaked env var for this process only.
  $env:BUILDMESH_PREFILL = $null
  $manifest = Join-Path $repo 'src-tauri\Cargo.toml'
  Push-Location $repo
  # Same PowerShell-5.1 NativeCommandError trap as Invoke-Unit: cargo's
  # "   Compiling …" progress lines arrive on stderr and would otherwise
  # become terminating errors under the script-wide `Stop` preference.
  # Locally downgrade for the cargo invocation; pass/fail still tracks
  # $LASTEXITCODE so a real test failure surfaces as a rust failure.
  $prevPref = $ErrorActionPreference
  try {
    $ErrorActionPreference = 'Continue'
    if ($CleanRust) {
      & cargo clean -p buildmesh --manifest-path $manifest
    }
    $cargoArgs = @('test', '--manifest-path', $manifest)
    if ($SerialRust) { $cargoArgs += @('--', '--test-threads=1') }
    & cargo @cargoArgs
  } finally {
    $ErrorActionPreference = $prevPref
    Pop-Location
  }
  if ($LASTEXITCODE -ne 0) { $script:failed += 'rust' }
}

if ($Target -in @('rust', 'all')) { Ensure-MobileBuilt }
if ($Target -in @('unit', 'all', 'all-ts')) { Invoke-Unit }
# Issue #1257 — integration must run in the default green bar (`all`)
# as well as `integration` and `all-ts`, otherwise a developer who only
# runs `scripts\check.ps1` locally gets a green bar that CI will reject.
if ($Target -in @('integration', 'all', 'all-ts')) { Invoke-Integration }
if ($Target -in @('all-ts')) { Invoke-TsBuild }
if ($Target -in @('rust', 'all')) { Invoke-Rust }

if ($failed.Count -gt 0) {
  Write-Host ("== FAIL: " + ($failed -join ', ') + " ==") -ForegroundColor Red
  exit 1
}
Write-Host '== GREEN ==' -ForegroundColor Green
exit 0
