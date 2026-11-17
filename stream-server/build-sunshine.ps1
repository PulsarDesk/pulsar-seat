# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the Pulsar Seat stream server: upstream Sunshine (GPL-3.0) at the commit pinned in
# stream-server/sunshine.json plus the patch set in stream-server/sunshine-patches/.
#
#  1. Fetch the pinned commit by SHA (shallow, git over the network) into
#     target\wf\sunshine-src, with the submodules listed in the pin file.
#  2. Reset the tree to that commit and apply every patch in name order (git apply --check,
#     then git apply --index). -Check stops here.
#  3. Toolchain: upstream's documented MSYS2 environment (UCRT64 for x64, CLANGARM64 for
#     ARM64). An existing MSYS2 is used when -Msys2Root (or PULSAR_MSYS2_ROOT) names one;
#     otherwise the pinned msys2-base archive is downloaded, SHA-256 checked and extracted
#     to target\wf\msys2 only. Nothing is installed system-wide; no PATH or registry change
#     (the MSYS2 shell gets its environment for this process only).
#  4. Pre-seed upstream's FFmpeg download with the pinned archive (SHA-256 checked), then
#     cmake + ninja (web UI, docs and tests off), copy sunshine.exe to
#     target\wf\sunshine-out\<arch>\ and write a build record and a runtime manifest
#     (flavour pulsar_fork) next to it.
#
# Every step logs to target\wf\sunshine-out\<arch>\build.log. Never signs, never installs.
#
#   powershell -ExecutionPolicy Bypass -File stream-server\build-sunshine.ps1 -Check
#   powershell -ExecutionPolicy Bypass -File stream-server\build-sunshine.ps1 -Arch x64
#   powershell -ExecutionPolicy Bypass -File stream-server\build-sunshine.ps1 -Arch arm64 -Msys2Root C:\msys64
param(
    [ValidateSet("x64", "arm64")] [string] $Arch = "x64",
    # Fetch + apply (git apply --check) only.
    [switch] $Check,
    # Fetch, apply and prepare MSYS2, but do not configure / compile.
    [switch] $NoBuild,
    [string] $Msys2Root = "",
    [string] $SourceDir = "",
    [string] $OutDir = "",
    [int] $Jobs = 0,
    # ninja -k 0: report every compile error, not only the first.
    [switch] $KeepGoing
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$pin = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot "sunshine.json") | ConvertFrom-Json
$patchDir = Join-Path $PSScriptRoot "sunshine-patches"
if (-not $SourceDir) { $SourceDir = Join-Path $repo "target\wf\sunshine-src" }
if (-not $OutDir) { $OutDir = Join-Path $repo "target\wf\sunshine-out\$Arch" }
$wf = Join-Path $repo "target\wf"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$log = Join-Path $OutDir "build.log"
"== build-sunshine $(Get-Date -Format o) arch=$Arch" | Out-File -FilePath $log -Encoding utf8

function Log([string] $m) {
    Write-Host $m
    $m | Out-File -FilePath $log -Append -Encoding utf8
}

function Invoke-Native([string] $exe, [string[]] $argv, [string] $what, [switch] $AllowFailure) {
    Log "> $what"
    # Windows PowerShell turns native stderr lines into errors under "Stop": only the exit
    # code decides here. The log stays UTF-8 (Tee-Object would write UTF-16 there).
    $old = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $exe @argv 2>&1 | ForEach-Object {
            $line = "$_"
            Write-Host $line
            Add-Content -LiteralPath $log -Value $line -Encoding utf8
        }
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $old }
    if ($code -ne 0) {
        if ($AllowFailure) { Log "($what exit $code, continuing)"; return }
        throw "$what failed (exit $code); see $log"
    }
}

function Get-Sha256([string] $path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
}

function Get-Pinned([string] $url, [string] $sha256, [string] $dest) {
    if ((Test-Path -LiteralPath $dest) -and (Get-Sha256 $dest) -eq $sha256) {
        Log "cached: $dest"
        return
    }
    Log "download: $url"
    $tmp = "$dest.part"
    New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
    $old = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $tmp } finally { $ProgressPreference = $old }
    $got = Get-Sha256 $tmp
    if ($got -ne $sha256) {
        Remove-Item -Force -LiteralPath $tmp
        throw "SHA-256 mismatch for $url (got $got, pinned $sha256)"
    }
    Move-Item -Force -LiteralPath $tmp -Destination $dest
}

# ---- 1. fetch by SHA ----
$git = (Get-Command git -ErrorAction Stop).Source
$commit = $pin.upstream.commit
if (-not (Test-Path -LiteralPath (Join-Path $SourceDir ".git"))) {
    New-Item -ItemType Directory -Force -Path $SourceDir | Out-Null
    Invoke-Native $git @("-C", $SourceDir, "init", "-q") "git init"
    Invoke-Native $git @("-C", $SourceDir, "remote", "add", "origin", $pin.upstream.repo) "git remote add"
}
$have = ""
try { $have = (& $git -C $SourceDir rev-parse "$commit^{commit}" 2>$null) } catch { $have = "" }
if ($have -ne $commit) {
    Invoke-Native $git @("-C", $SourceDir, "fetch", "-q", "--depth", "1", "origin", $commit) "git fetch $commit ($($pin.upstream.tag))"
}

# ---- 2. clean tree at the pinned commit + patches ----
Invoke-Native $git @("-C", $SourceDir, "-c", "advice.detachedHead=false", "checkout", "-q", "-f", "--detach", $commit) "git checkout $commit"
Invoke-Native $git @("-C", $SourceDir, "clean", "-q", "-fdx", "-e", "build-*/") "git clean"
$subs = @($pin.upstream.submodules)
Invoke-Native $git (@("-C", $SourceDir, "submodule", "update", "--init", "--depth", "1", "--recommend-shallow", "--") + $subs) "git submodule update"
# Nested submodules (enet, simde, ...) of every listed submodule except build-deps, whose
# own submodules are the dependency sources it prebuilds (FFmpeg comes pinned below).
$recursive = @($subs | Where-Object { $pin.upstream.no_recursion -notcontains $_ })
Invoke-Native $git (@("-C", $SourceDir, "submodule", "update", "--init", "--recursive", "--depth", "1", "--recommend-shallow", "--") + $recursive) "git submodule update --recursive"

$patches = @(Get-ChildItem -LiteralPath $patchDir -Filter "*.patch" | Sort-Object Name)
if ($patches.Count -eq 0) { throw "no patches in $patchDir" }
foreach ($p in $patches) {
    Invoke-Native $git @("-C", $SourceDir, "apply", "--check", "--index", $p.FullName) "git apply --check $($p.Name)"
    Invoke-Native $git @("-C", $SourceDir, "apply", "--index", $p.FullName) "git apply $($p.Name)"
}
Log "applied $($patches.Count) patches on $commit"
if ($Check) { Log "check only: done"; return }

# ---- 3. MSYS2 ----
$a = $pin.arches.$Arch
if (-not $Msys2Root -and $env:PULSAR_MSYS2_ROOT) { $Msys2Root = $env:PULSAR_MSYS2_ROOT }
if (-not $Msys2Root) {
    $Msys2Root = Join-Path $wf "msys2\msys64"
    $bash = Join-Path $Msys2Root "usr\bin\bash.exe"
    if (-not (Test-Path -LiteralPath $bash)) {
        $archive = Join-Path $wf "msys2-dl\$(Split-Path -Leaf $pin.msys2.url)"
        Get-Pinned $pin.msys2.url $pin.msys2.sha256 $archive
        $dest = Join-Path $wf "msys2"
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        # Windows' own tar (libarchive) reads .tar.xz.
        Invoke-Native "$env:SystemRoot\System32\tar.exe" @("-xf", $archive, "-C", $dest) "extract MSYS2 base"
    }
}
$bash = Join-Path $Msys2Root "usr\bin\bash.exe"
if (-not (Test-Path -LiteralPath $bash)) { throw "no MSYS2 at $Msys2Root" }
Log "MSYS2: $Msys2Root ($($a.msystem))"

# Environment for the MSYS2 shell of this process only.
$env:MSYSTEM = $a.msystem
$env:CHERE_INVOKING = "1"
$env:MSYS2_PATH_TYPE = "minimal"

function Invoke-Msys([string] $script, [string] $what, [switch] $AllowFailure) {
    Invoke-Native $bash @("-lc", $script) $what -AllowFailure:$AllowFailure
}

# First start creates the home directory and the pacman keyring.
Invoke-Msys "true" "MSYS2 first start"
# Core update, then the rest (pacman may need two passes).
# The core update ends every MSYS2 process, including this shell: its exit code is ignored.
Invoke-Msys "pacman -Syuu --noconfirm --disable-download-timeout" "pacman -Syuu (1)" -AllowFailure
Invoke-Msys "pacman -Syuu --noconfirm --disable-download-timeout" "pacman -Syuu (2)"
$pkgs = @($a.packages | ForEach-Object { "mingw-w64-$($a.toolchain)-$_" }) + @("git")
Invoke-Msys ("pacman -S --needed --noconfirm --disable-download-timeout " + ($pkgs -join " ")) "pacman -S toolchain + dependencies"
$ErrorActionPreference = "Continue"
$versions = @(& $bash -lc ("pacman -Q " + ($pkgs -join " ")) 2>&1 | ForEach-Object { "$_" })
$ErrorActionPreference = "Stop"
if ($NoBuild) { Log "no build: done"; return }

# ---- 4. FFmpeg pin, configure, build ----
$build = Join-Path $SourceDir "build-$Arch"
$ffDir = Join-Path $build "_deps\ffmpeg-$($pin.ffmpeg_tag)"
$ffName = "Windows-$($a.ffmpeg.cmake_processor)-ffmpeg.tar.gz"
Get-Pinned $a.ffmpeg.url $a.ffmpeg.sha256 (Join-Path $ffDir $ffName)

function To-Msys([string] $p) {
    $full = [System.IO.Path]::GetFullPath($p)
    "/" + $full.Substring(0, 1).ToLowerInvariant() + ($full.Substring(2) -replace "\\", "/")
}
$src = To-Msys $SourceDir
$bld = To-Msys $build
$j = if ($Jobs -gt 0) { "-j $Jobs" } else { "" }
$cmakeArgs = @(
    "-B '$bld'", "-S '$src'", "-G Ninja",
    "-DCMAKE_BUILD_TYPE=Release",
    "-DBUILD_DOCS=OFF", "-DBUILD_TESTS=OFF",
    "-DSUNSHINE_BUILD_WEB_UI=OFF",
    "-DSUNSHINE_PUBLISHER_NAME='Pulsar Seat'",
    "-DSUNSHINE_PUBLISHER_WEBSITE=''",
    "-DSUNSHINE_PUBLISHER_ISSUE_URL=''"
) -join " "
# Per-arch compiler flags from the pin file (workarounds for third-party submodule code).
if ($a.PSObject.Properties.Name -contains "cxx_flags") { $cmakeArgs += " -DCMAKE_CXX_FLAGS='$($a.cxx_flags)'" }
# Upstream CI variables: numeric version for the Windows resource, branch and commit for the log.
$serverVersion = "$($pin.upstream.tag)-pulsar.$($pin.patch_version)"
$env:BUILD_VERSION = $pin.upstream.tag
$env:BRANCH = "pulsar-seat"
$env:COMMIT = $commit
Invoke-Msys "cmake $cmakeArgs" "cmake configure"
$k = if ($KeepGoing) { "-k 0" } else { "" }
Invoke-Msys "ninja -C '$bld' $j $k sunshine" "ninja sunshine"

$exe = Join-Path $build "sunshine.exe"
if (-not (Test-Path -LiteralPath $exe)) { throw "build produced no sunshine.exe" }
Copy-Item -Force -LiteralPath $exe -Destination (Join-Path $OutDir "sunshine.exe")
# ARM64: upstream links MinHook as minhook-detours (a DLL, fetched by upstream's cmake with a
# pinned hash); it must sit next to the exe (the upstream ARM64 portable ships it too).
$extra = @(Get-ChildItem -LiteralPath (Join-Path $build "_deps\minhook-detours-src\Release") -Filter "minhook-detours.ARM64.Release.dll" -ErrorAction SilentlyContinue)
if ($Arch -eq "arm64") {
    if ($extra.Count -ne 1) { throw "minhook-detours.ARM64.Release.dll not found in the build" }
    Copy-Item -Force -LiteralPath $extra[0].FullName -Destination $OutDir
}
$sha = Get-Sha256 (Join-Path $OutDir "sunshine.exe")

# PE machine check (0x8664 x64, 0xAA64 ARM64).
$bytes = [System.IO.File]::ReadAllBytes((Join-Path $OutDir "sunshine.exe"))
$peOff = [BitConverter]::ToInt32($bytes, 0x3C)
$machine = [BitConverter]::ToUInt16($bytes, $peOff + 4)
$want = if ($Arch -eq "x64") { 0x8664 } else { 0xAA64 }
if ($machine -ne $want) { throw ("sunshine.exe has PE machine 0x{0:X4}, expected 0x{1:X4}" -f $machine, $want) }

$patchHashes = @($patches | ForEach-Object { [ordered]@{ name = $_.Name; sha256 = (Get-Sha256 $_.FullName) } })
$record = [ordered]@{
    upstream_tag = $pin.upstream.tag
    upstream_commit = $commit
    patches = $patchHashes
    arch = $Arch
    msystem = $a.msystem
    msys2_packages = $versions
    sunshine_sha256 = $sha
    built_at = (Get-Date -Format o)
}
[System.IO.File]::WriteAllText((Join-Path $OutDir "build-record.json"), ($record | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding $false))
# Runtime manifest (seat-stream dist::ServerDist); fetch-deps / check-payload use it once a
# pinned release of this build exists.
$manifest = [ordered]@{
    flavour = "pulsar_fork"
    version = $serverVersion
    file = "sunshine.exe"
    sha256 = $sha
    source_url = "$($pin.upstream.repo -replace '\.git$', '')/tree/$($pin.upstream.tag) + stream-server/sunshine-patches"
    licence = "GPL-3.0-only"
}
[System.IO.File]::WriteAllText((Join-Path $OutDir "stream-server.json"), ($manifest | ConvertTo-Json), (New-Object System.Text.UTF8Encoding $false))
Log ("built sunshine.exe ({0}, PE 0x{1:X4}) sha256 {2}" -f $Arch, $machine, $sha)
