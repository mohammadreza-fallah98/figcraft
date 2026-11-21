#requires -Version 7
<#
  One-command daily update of the app (folder figcraft) from the upstream creator's newest release.
  Design: branch `release-rebranded` is a linear mirror of upstream, each commit = one upstream commit run through
  rebrand.ps1 (in a temp worktree). Merging that branch into dev keeps the 3-way merge base correct, so only real
  upstream changes (not 1500 renames) reach dev. See README.txt.
  Usage: pwsh figiran-tools\rebrand\update-from-upstream.ps1 [-Ref upstream/release] [-Push] [-NoCheck] [-Repo <path>] [-Branch dev]
  Exit: 0 = done (or nothing new), 1 = conflicts/failure (repo left in the merge state when conflicted).
#>
[CmdletBinding()]
param(
  [string]$Ref = 'upstream/release',
  [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,   # this repository
  [string]$Branch = 'dev',
  [string]$Mirror = 'release-rebranded',
  [string]$BootstrapBase,          # first run only: upstream commit that Branch last merged (default: merge-base of Branch and Ref)
  [string]$CargoTargetDir,         # default: build-cache (same as build-app.bat)
  [switch]$NoCheck,                # skip cargo check
  [switch]$Push                    # push the app repo (default: print the command only)
)
$ErrorActionPreference = 'Stop'
$rebrand = Join-Path $PSScriptRoot 'rebrand.ps1'
$Repo = (Resolve-Path $Repo).Path
$Portal = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path   # this repository's root
$licenseRx = [regex]::new('(?i)(^|/)(LICEN[SC]E|COPYING|NOTICE|UNLICENSE)([._-][^/]*)?$|(^|/)[^/]*-LICENSE[^/]*$')

function G { & git -C $Repo @args; if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') failed ($LASTEXITCODE)" } }
function GQ { $o = & git -C $Repo @args 2>$null; if ($LASTEXITCODE -ne 0) { return $null }; $o }
function Say([string]$m) { Write-Host $m }
function Fail([string]$m) { Write-Host "ERROR: $m" -ForegroundColor Red; exit 1 }

# ---- preconditions ---------------------------------------------------------------------------------
if (-not (Test-Path (Join-Path $Repo '.git'))) { Fail "not a git repo: $Repo" }
if (-not (GQ remote get-url upstream)) { Fail "remote 'upstream' is missing in $Repo" }
$gitDir = (GQ rev-parse --absolute-git-dir)
if (Test-Path (Join-Path $gitDir 'MERGE_HEAD')) { Fail 'a merge is already in progress; finish it (git commit) or abort it (git merge --abort) first' }
if (Test-Path (Join-Path $gitDir 'rebase-merge')) { Fail 'a rebase is in progress' }
if (GQ status --porcelain) { Fail "uncommitted changes in $Repo (commit or stash them first)" }
$cur = GQ rev-parse --abbrev-ref HEAD
if ($cur -ne $Branch) { Say "switching $cur -> $Branch"; G switch $Branch }

# ---- fetch -----------------------------------------------------------------------------------------
Say 'fetching upstream (heads + tags)...'
G fetch upstream --tags
$upSha = GQ rev-parse --verify "$Ref^{commit}"
if (-not $upSha) { Fail "cannot resolve $Ref" }
$upSha = $upSha.Trim()
$label = (GQ describe --tags --always $upSha); $label = "$label".Trim()
Say "upstream $Ref = $($upSha.Substring(0,8)) ($label)"

# ---- advance the rebranded mirror ------------------------------------------------------------------
$tip = GQ rev-parse --verify --quiet "refs/heads/$Mirror^{commit}"
if (-not $tip) {   # the mirror branch was deleted: restore it from the newest mirror commit that dev already contains (it carries the Upstream-Commit trailer)
  $last = GQ log $Branch --format=%H --grep='^Upstream-Commit: ' -n 1
  if ($last) { $last = "$last".Trim(); G branch $Mirror $last; $tip = $last; Say "restored $Mirror at $($last.Substring(0,8)) from the history of $Branch" }
}
$already = $false
if ($tip) { $already = [bool](GQ log $Mirror --format=%H --grep="^Upstream-Commit: $upSha$" -n 1) }
if ($already) { Say "$Mirror already contains $($upSha.Substring(0,8)); nothing to mirror." }
else {
  if ($tip) { $base = $upSha; $parentArg = @('-p', $tip.Trim()); Say "advancing $Mirror from $($tip.Trim().Substring(0,8))" }
  else {
    $base = if ($BootstrapBase) { (GQ rev-parse --verify "$BootstrapBase^{commit}").Trim() } else { GQ merge-base $Branch $upSha }
    if (-not $base) { Fail "bootstrap: no common base between $Branch and $Ref" }
    $base = "$base".Trim(); $parentArg = @(); Say "bootstrap: creating $Mirror from upstream $($base.Substring(0,8))"
  }
  $wt = Join-Path ([IO.Path]::GetTempPath()) ("figcraft-rebrand-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
  G worktree add --detach $wt $base
  try {
    & pwsh -NoProfile -File $rebrand -AppOnly -Root $wt
    if ($LASTEXITCODE -ne 0) { throw "rebrand.ps1 reported residuals on the upstream snapshot (exit $LASTEXITCODE); update the script's rules" }
    & git -C $wt add -A; if ($LASTEXITCODE -ne 0) { throw 'git add failed in the worktree' }
    $tree = (& git -C $wt write-tree).Trim()
    $msg = "chore(upstream): mirror upstream $label rebranded`n`nUpstream-Commit: $base"
    $new = (& git -C $Repo commit-tree $tree @parentArg -m $msg).Trim()
    if (-not $new) { throw 'commit-tree failed' }
    if ($tip -and ((& git -C $Repo rev-parse "$new^{tree}").Trim() -eq (& git -C $Repo rev-parse "$($tip.Trim())^{tree}").Trim())) {
      Say 'rebranded tree identical to the previous mirror tip; no new commit.'
    } else {
      G update-ref "refs/heads/$Mirror" $new $(if ($tip) { $tip.Trim() } else { '' })
      Say "$Mirror -> $($new.Substring(0,8))"
      if (-not $tip) {   # bootstrap: make the baseline an ancestor of dev (tree unchanged), so later merges use it as the 3-way base
        G merge -s ours --allow-unrelated-histories --no-edit -m "chore(upstream): record the rebranded upstream baseline $label" $new
        Say "recorded the baseline in $Branch (merge -s ours, tree unchanged)"
      }
    }
  } finally {
    & git -C $Repo worktree remove --force $wt 2>$null
    if (Test-Path $wt) { Remove-Item $wt -Recurse -Force -ErrorAction SilentlyContinue }
    & git -C $Repo worktree prune
  }
  # bootstrap with an older base: continue with the requested ref in the same run
  if (-not $tip -and $base -ne $upSha) { Say "bootstrap done at $($base.Substring(0,8)); re-running for $($upSha.Substring(0,8))"; & pwsh -NoProfile -File $PSCommandPath -Ref $upSha -Repo $Repo -Branch $Branch -Mirror $Mirror -NoCheck:$NoCheck -Push:$Push -CargoTargetDir $CargoTargetDir; exit $LASTEXITCODE }
}

# ---- merge into dev --------------------------------------------------------------------------------
if (-not (GQ merge-base $Branch $Mirror)) { Fail "$Mirror shares no history with $Branch; delete the branch (git branch -D $Mirror) and run again to bootstrap" }
$env:GIT_EDITOR = 'true'; $env:GIT_MERGE_AUTOEDIT = 'no'
$before = (GQ rev-parse HEAD).Trim()
& git -C $Repo merge --no-edit $Mirror 2>&1 | ForEach-Object { Say "  $_" }
$mergeRc = $LASTEXITCODE
if ($mergeRc -ne 0) {
  $un = @(GQ diff --name-only --diff-filter=U)
  foreach ($f in $un | Where-Object { $licenseRx.IsMatch($_) }) { Say "  licence file conflict -> deleted: $f"; & git -C $Repo rm -q -f -- $f 2>$null }
  $left = @(GQ diff --name-only --diff-filter=U)
  if ($left.Count -gt 0) {
    Write-Host "MERGE CONFLICTS: $($left.Count) file(s)" -ForegroundColor Red
    $left | ForEach-Object { Say "   $_" }
    Say "The repo ($Repo) is left in the merge state. Resolve each file, then:"
    Say "   git -C `"$Repo`" add <files>; git -C `"$Repo`" commit --no-edit"
    Say "   (or git -C `"$Repo`" merge --abort to undo), then run this script again to run the checks."
    exit 1
  }
  & git -C $Repo commit --no-edit 2>&1 | ForEach-Object { Say "  $_" }
  if ($LASTEXITCODE -ne 0) { Fail 'merge commit failed' }
}
$after = (GQ rev-parse HEAD).Trim()
$changed = if ($before -ne $after) { @(GQ diff --name-only $before $after).Count } else { 0 }
Say "merge done: $($before.Substring(0,8)) -> $($after.Substring(0,8)), files changed: $changed"

# ---- checks ----------------------------------------------------------------------------------------
& pwsh -NoProfile -File $rebrand -AppOnly -VerifyOnly -Root $Repo | Select-Object -Last 8 | ForEach-Object { Say "  $_" }
if ($LASTEXITCODE -ne 0) { Fail 'rebrand verification found legacy names (see above); fix, commit, re-run' }
if (-not $NoCheck) {
  $msvc = 'C:\Program Files\Microsoft Visual Studio\18\Enterprise\VC\Tools\MSVC\14.50.35717'; $xwin = Join-Path $env:USERPROFILE 'xwin\splat'
  $env:PATH = "$msvc\bin\Hostx64\x64;$env:USERPROFILE\.cargo\bin;$env:PATH"
  $env:LIB = "$msvc\lib\x64;$xwin\crt\lib\x86_64;$xwin\sdk\lib\um\x86_64;$xwin\sdk\lib\ucrt\x86_64"
  $env:INCLUDE = "$xwin\crt\include;$xwin\sdk\include\ucrt;$xwin\sdk\include\um;$xwin\sdk\include\shared"
  $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = "$msvc\bin\Hostx64\x64\link.exe"
  $env:CARGO_TARGET_DIR = if ($CargoTargetDir) { $CargoTargetDir } else { Join-Path $Portal 'build-cache' }
  Say "cargo check --release (target $($env:CARGO_TARGET_DIR))..."
  & cargo check --release --manifest-path (Join-Path $Repo 'Cargo.toml') -p figiran-portal -p figiran-ui-egui -p figiran 2>&1 | Select-Object -Last 6 | ForEach-Object { Say "  $_" }; $rc = $LASTEXITCODE
  if ($rc -ne 0) { Fail "cargo check failed ($rc)" }
}

# ---- finish ----------------------------------------------------------------------------------------
if ($before -eq $after) { Say 'dev already up to date with upstream.' }
if ($Push) {
  Say 'pushing...'
  & git -C $Repo push origin $Branch; if ($LASTEXITCODE -ne 0) { Fail 'push of the app repo failed' }
  Say 'DONE and pushed.'
} else {
  Say 'DONE (nothing pushed). To publish:'
  Say "   git -C `"$Repo`" push origin $Branch"
  Say '   (or run again with -Push)'
}
exit 0
