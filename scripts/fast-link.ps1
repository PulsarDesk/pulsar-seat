# SPDX-License-Identifier: GPL-3.0-only
#
# Opt-in fast linker for debug / test builds: rust-lld (LLVM's lld-link flavour, shipped
# with every rustup toolchain in <sysroot>\lib\rustlib\<host>\bin) instead of MSVC link.exe,
# for the Windows MSVC targets of this repo. Dot-source it, so it changes this shell only:
#
#   . .\scripts\fast-link.ps1          # on:  sets CARGO_TARGET_<TRIPLE>_LINKER=rust-lld.exe
#   . .\scripts\fast-link.ps1 -Off     # off: removes those variables again
#
# Never for release binaries: build-installer.ps1, build-setup.ps1 and build-drivers.ps1
# drop these variables for their own run, so a shell with the switch on still builds the
# payload with link.exe. ci-local.ps1 -FastLink sets them for its debug gates only. Cargo
# keys its build cache on the linker, so switching it on or off rebuilds a target dir's
# artifacts once; keep one target dir per setting (e.g. CARGO_TARGET_DIR=target\wf-lld).
param([switch] $Off)

$fastLinkTriples = @("aarch64-pc-windows-msvc", "x86_64-pc-windows-msvc", "i686-pc-windows-msvc")
foreach ($t in $fastLinkTriples) {
    $name = "CARGO_TARGET_$($t.ToUpperInvariant().Replace('-', '_'))_LINKER"
    if ($Off) {
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    } else {
        Set-Item "Env:$name" "rust-lld.exe"
    }
}
if ($Off) {
    Write-Host "fast link off: cargo links with MSVC link.exe"
} else {
    $sysroot = (& rustc --print sysroot).Trim()
    $hostTriple = ((& rustc -vV) | Where-Object { $_ -like "host: *" }).Substring(6).Trim()
    $lld = Join-Path $sysroot "lib\rustlib\$hostTriple\bin\rust-lld.exe"
    if (-not (Test-Path $lld)) { Write-Warning "rust-lld.exe not found at $lld; run '. .\scripts\fast-link.ps1 -Off'" }
    Write-Host "fast link on (debug / test builds; release scripts ignore it): rust-lld.exe for $($fastLinkTriples -join ', ')"
}
