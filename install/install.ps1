<#
.SYNOPSIS
Velra installer for Windows.

.DESCRIPTION
    powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1 | iex"

To also register the hooks:
    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1))) -Enable

Environment:
    VELRA_VERSION          version to install (default: latest release)
    VELRA_HOME             install root (default: $env:USERPROFILE\.velra)
    VELRA_NO_MODIFY_PATH=1 do not modify the user PATH
    VELRA_DOWNLOAD_BASE    archive source: an https:// base URL or a local
                           directory (used by tests)

No administrator rights are required and nothing runs unless you pass -Enable.
#>
[CmdletBinding()]
param(
    [switch]$Enable,
    [switch]$NoModifyPath
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Fail($message) {
    Write-Host $message -ForegroundColor Red
    exit 1
}

$repo = if ($env:VELRA_REPO) { $env:VELRA_REPO } else { 'aryanghai12/velra' }
$releases = if ($env:VELRA_DOWNLOAD_BASE) { $env:VELRA_DOWNLOAD_BASE } else { "https://github.com/$repo/releases" }
$installRoot = if ($env:VELRA_HOME) { $env:VELRA_HOME } else { Join-Path $env:USERPROFILE '.velra' }
$binDir = Join-Path $installRoot 'bin'
$version = if ($env:VELRA_VERSION) { $env:VELRA_VERSION } else { 'latest' }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { $null }
}
if (-not $arch) { Fail "velra: unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
$target = "$arch-pc-windows-msvc"
$archive = "velra-$target.zip"

$base = if ($releases -like 'http*') {
    if ($version -eq 'latest') { "$releases/latest/download" } else { "$releases/download/v$($version.TrimStart('v'))" }
} else {
    $releases
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("velra-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    $archivePath = Join-Path $tmp $archive
    $sumPath = "$archivePath.sha256"

    Write-Host "Downloading velra ($target)..."
    if ($base -like 'http*') {
        try {
            Invoke-WebRequest -Uri "$base/$archive" -OutFile $archivePath -UseBasicParsing
            Invoke-WebRequest -Uri "$base/$archive.sha256" -OutFile $sumPath -UseBasicParsing
        } catch {
            Fail "velra: download failed: $base/$archive`n  $($_.Exception.Message)"
        }
    } else {
        Copy-Item (Join-Path $base $archive) $archivePath
        Copy-Item (Join-Path $base "$archive.sha256") $sumPath
    }

    $expected = ((Get-Content $sumPath -Raw) -split '\s+')[0].Trim().ToLower()
    $actual = (Get-FileHash $archivePath -Algorithm SHA256).Hash.ToLower()
    if (-not $expected -or $expected -ne $actual) {
        Fail "velra: checksum mismatch for $archive`n  expected: $expected`n  actual:   $actual`nRefusing to install. The download may be corrupt or tampered with."
    }

    Expand-Archive -Path $archivePath -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter 'velra.exe' | Select-Object -First 1
    if (-not $exe) { Fail "velra: archive did not contain velra.exe" }

    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    # Atomic replace: copy beside the target, then move over it.
    $staged = Join-Path $binDir 'velra.exe.new'
    Copy-Item $exe.FullName $staged -Force
    Move-Item $staged (Join-Path $binDir 'velra.exe') -Force

    $installed = & (Join-Path $binDir 'velra.exe') --version 2>$null
    $installedVersion = if ($installed) { ($installed -split '\s+')[1] } else { '' }

    if (-not $NoModifyPath -and $env:VELRA_NO_MODIFY_PATH -ne '1') {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if (($userPath -split ';') -notcontains $binDir) {
            $newPath = if ([string]::IsNullOrEmpty($userPath)) { $binDir } else { "$userPath;$binDir" }
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Write-Host "  Added $binDir to your user PATH (restart your terminal to pick it up)"
        }
        $env:Path = "$binDir;$env:Path"
    }

    if ($Enable) {
        & (Join-Path $binDir 'velra.exe') enable
        if ($LASTEXITCODE -ne 0) { Fail 'velra: `velra enable` failed' }
    }

    Write-Host ''
    Write-Host "$([char]0x2713) velra $installedVersion installed to $binDir\velra.exe"
    if ($Enable) {
        Write-Host 'Next: keep coding - run `velra inspect` any time.'
    } else {
        Write-Host 'Next: velra enable'
    }
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
