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
  unit | rust | all  (default: all)

.EXAMPLE
  scripts\check.ps1            # full green bar (mobile build + unit + rust)
  scripts\check.ps1 unit       # just the TS unit suite, correct pool
  scripts\check.ps1 rust -SerialRust
#>
[CmdletBinding()]
param(
  [ValidateSet('unit', 'rust', 'all')]
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

# Same shadowing class for openssl.exe: devkitPro's MSYS2 copy crashes on
# startup ("add_item ... failed, errno 1" cygwin heap error), failing the 4
# http::tls chain-verification tests that shell out to `openssl`. Pin Git for
# Windows' usr\bin (which ships a working OpenSSL but no git.exe, so the git
# pin above is unaffected) ahead of it.
$gitUsrBin = 'C:\Program Files\Git\usr\bin'
$opensslOnPath = (Get-Command openssl -ErrorAction SilentlyContinue).Source
if ((Test-Path (Join-Path $gitUsrBin 'openssl.exe')) -and ($opensslOnPath -notlike "$gitUsrBin*")) {
  Write-Host "== PATH openssl is '$opensslOnPath' -> pinning $gitUsrBin first ==" -ForegroundColor Yellow
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
  try { & npx vitest run --pool=threads tests/unit } finally { Pop-Location }
  if ($LASTEXITCODE -ne 0) { $script:failed += 'unit' }
}

function Invoke-Rust {
  Write-Host '== rust (cargo test) ==' -ForegroundColor Cyan
  # Clear the leaked env var for this process only.
  $env:BUILDMESH_PREFILL = $null
  $manifest = Join-Path $repo 'src-tauri\Cargo.toml'
  Push-Location $repo
  try {
    if ($CleanRust) {
      & cargo clean -p buildmesh --manifest-path $manifest
    }
    $cargoArgs = @('test', '--manifest-path', $manifest)
    if ($SerialRust) { $cargoArgs += @('--', '--test-threads=1') }
    & cargo @cargoArgs
  } finally { Pop-Location }
  if ($LASTEXITCODE -ne 0) { $script:failed += 'rust' }
}

if ($Target -in @('rust', 'all')) { Ensure-MobileBuilt }
if ($Target -in @('unit', 'all')) { Invoke-Unit }
if ($Target -in @('rust', 'all')) { Invoke-Rust }

if ($failed.Count -gt 0) {
  Write-Host ("== FAIL: " + ($failed -join ', ') + " ==") -ForegroundColor Red
  exit 1
}
Write-Host '== GREEN ==' -ForegroundColor Green
exit 0
