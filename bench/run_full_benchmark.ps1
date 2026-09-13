<#
.SYNOPSIS
  Run the whole Velra benchmark suite on Windows.

.DESCRIPTION
  A thin wrapper around bench/run_full_benchmark.py. It puts the portable
  WinLibs MinGW toolchain on PATH first, because the release build links
  bundled SQLite and needs dlltool.exe; without it cargo fails with
  "error calling dlltool 'dlltool.exe': program not found".

  Every argument is passed straight through, so:

      .\bench\run_full_benchmark.ps1 -Args '--replicates','1'

.EXAMPLE
  .\bench\run_full_benchmark.ps1
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $Args
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$winlibs = Join-Path $env:LOCALAPPDATA 'Programs\winlibs-mingw64\mingw64\bin'
if (Test-Path $winlibs) {
    $env:Path = "$winlibs;$env:Path"
}
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) {
    $env:Path = "$cargoBin;$env:Path"
}

$python = (Get-Command python -ErrorAction SilentlyContinue)?.Source
if (-not $python) { $python = (Get-Command python3 -ErrorAction SilentlyContinue)?.Source }
if (-not $python) { throw 'Python 3.11+ is required and was not found on PATH.' }

& $python (Join-Path $PSScriptRoot 'run_full_benchmark.py') @Args
exit $LASTEXITCODE
