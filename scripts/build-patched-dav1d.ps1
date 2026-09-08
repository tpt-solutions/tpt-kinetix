<#
.SYNOPSIS
  Clone + patch + build a reference dav1d whose decoder prints a full
  per-block trace (prediction, dequantized coefficients, reconstruction)
  to stdout. This is the "patched dav1d" every AV1 reconstruction-bug
  hunt in todo-av1.md has been blocked on -- prior sessions could not
  build one on this Windows box.

.DESCRIPTION
  Requires: git, meson + ninja (pip --user is fine), and Visual Studio
  2022 (Community) for the MSVC toolchain. No MSYS2/mingw/nasm needed
  (asm is disabled -- the C paths are bit-exact anyway).

  Output binary:  <OutDir>\dav1d\build\tools\dav1d.exe
  (dav1d.dll is copied next to it so it runs from any cwd)

  Usage:
    dav1d.exe -i frame.obu -o out.y4m 1>trace.txt 2>/dev/null

  The trace uses dav1d's own DEBUG_BLOCK_INFO / DEBUG_B_PIXELS hooks
  (src/recon.h), flipped on by scripts/dav1d-blockdump.patch. Narrow the
  t->bx / t->by window in that patch for large clips.

  Pinned dav1d commit: aa09a630ef57ee7d9482ffb7ef355a903dbb5302
#>
param(
  [string]$OutDir = "$env:LOCALAPPDATA\Temp\tpt-kinetix-dav1d",
  [string]$VsPath = "C:\Program Files\Microsoft Visual Studio\2022\Community",
  [string]$Dav1dCommit = "aa09a630ef57ee7d9482ffb7ef355a903dbb5302"
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$patch = Join-Path $PSScriptRoot "dav1d-blockdump.patch"

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Set-Location $OutDir
if (-not (Test-Path "$OutDir\dav1d\.git")) {
  git clone https://code.videolan.org/videolan/dav1d.git
}
Set-Location "$OutDir\dav1d"
git fetch --depth 1 origin $Dav1dCommit 2>$null
git checkout -f $Dav1dCommit
git checkout -- .
git apply --whitespace=nowarn $patch
Write-Host "applied $patch"

& "$VsPath\Common7\Tools\Launch-VsDevShell.ps1" -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Null
$env:PATH = "$env:APPDATA\Python\Python313\Scripts;" + $env:PATH

if (Test-Path build) { Remove-Item -Recurse -Force build }
meson setup build --buildtype release -Denable_asm=false -Denable_tools=true -Denable_tests=false
ninja -C build
Copy-Item build\src\dav1d.dll build\tools\ -Force
Write-Host "`nBuilt: $OutDir\dav1d\build\tools\dav1d.exe"
