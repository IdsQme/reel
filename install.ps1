# Installs reel and everything it needs on Windows, and adds them to PATH.
# Safe to run again: it skips what is already there, and updates reel itself.
#
#   irm https://raw.githubusercontent.com/IdsQme/reel/main/install.ps1 | iex
#
# From a clone, run .\install.ps1. Add -WithFirefox to also install Firefox, which is only
# needed for the optional YouTube sign-in.
param([switch]$WithFirefox)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Toolchain = 'stable-x86_64-pc-windows-gnullvm'
$ArchiveUrl = 'https://github.com/IdsQme/reel/archive/refs/heads/main.zip'
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\reel'
$WingetPackages = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages'

function Write-Step($text) { Write-Host "`n==> $text" -ForegroundColor Magenta }
function Write-Done($text) { Write-Host "    $text" -ForegroundColor Green }

# Adds a folder to the user's PATH permanently and to this session. Reads and writes the raw
# registry value so entries such as %USERPROFILE%\bin are not rewritten.
function Add-ToUserPath($dir) {
    if (-not $dir -or -not (Test-Path $dir)) { return }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    try {
        $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $entries = @($raw -split ';' | Where-Object { $_ })
        $known = @($entries | ForEach-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') })
        if ($known -notcontains $dir.TrimEnd('\')) {
            $key.SetValue('Path', (($entries + $dir) -join ';'), [Microsoft.Win32.RegistryValueKind]::ExpandString)
            Write-Done "added to PATH: $dir"
            $script:PathChanged = $true
        }
    } finally {
        $key.Close()
    }
    if (($env:Path -split ';') -notcontains $dir) { $env:Path = "$dir;$env:Path" }
}

function Find-Tool($command, $extraPaths) {
    $found = Get-Command $command -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) { return $found.Source }
    foreach ($path in $extraPaths) {
        if ($path -and (Test-Path $path)) { return (Resolve-Path $path).Path }
    }
    return $null
}

function Find-InWingetPackages($package, $exe) {
    if (-not (Test-Path $WingetPackages)) { return $null }
    $found = Get-ChildItem $WingetPackages -Directory -Filter "$package*" -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ChildItem $_.FullName -Recurse -Filter $exe -ErrorAction SilentlyContinue } |
        Select-Object -First 1
    if ($found) { return $found.FullName }
    return $null
}

# Finds a tool, installing it with winget if it is missing, and puts its folder on PATH.
function Install-Tool($name, $id, $command, $extraPaths) {
    $path = Find-Tool $command $extraPaths
    if (-not $path) {
        Write-Host "    installing $name..."
        # winget also exits non-zero for "already installed", so the check below decides.
        & winget install --id $id -e --silent --accept-package-agreements --accept-source-agreements | Out-Null
        $path = Find-Tool $command $extraPaths
        if (-not $path) { $path = Find-InWingetPackages $id "$command.exe" }
        if (-not $path) { throw "$name did not install. Try: winget install --id $id -e" }
    } else {
        Write-Done "$name already installed"
    }
    Add-ToUserPath (Split-Path $path -Parent)
    return $path
}

if (-not $IsWindows -and $PSVersionTable.PSEdition -eq 'Core') { throw 'This script is for Windows.' }
if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw 'winget was not found. Install "App Installer" from the Microsoft Store, then run this again.'
}
$script:PathChanged = $false

Write-Step 'Player and YouTube tools'
Install-Tool 'mpv' 'shinchiro.mpv' 'mpv' @('C:\Program Files\MPV Player\mpv.exe', 'C:\Program Files\mpv\mpv.exe') | Out-Null
Install-Tool 'yt-dlp' 'yt-dlp.yt-dlp' 'yt-dlp' @((Find-InWingetPackages 'yt-dlp.yt-dlp' 'yt-dlp.exe')) | Out-Null
Install-Tool 'Deno' 'DenoLand.Deno' 'deno' @((Find-InWingetPackages 'DenoLand.Deno' 'deno.exe')) | Out-Null
if ($WithFirefox) {
    Install-Tool 'Firefox' 'Mozilla.Firefox' 'firefox' @('C:\Program Files\Mozilla Firefox\firefox.exe') | Out-Null
}

Write-Step 'Build tools'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
Install-Tool 'Rust' 'Rustlang.Rustup' 'rustup' @((Join-Path $cargoBin 'rustup.exe')) | Out-Null
Add-ToUserPath $cargoBin
& rustup toolchain install $Toolchain --profile minimal 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "could not install the Rust toolchain $Toolchain" }
# The linker and C compiler. MSYS2's works if it is there, so nothing extra is needed then.
$clang = Find-Tool 'x86_64-w64-mingw32-clang' @('C:\msys64\clang64\bin\x86_64-w64-mingw32-clang.exe', (Find-InWingetPackages 'MartinStorsjo.LLVM-MinGW' 'x86_64-w64-mingw32-clang.exe'))
if (-not $clang) {
    Write-Host '    installing LLVM-MinGW...'
    & winget install --id MartinStorsjo.LLVM-MinGW.UCRT -e --silent --accept-package-agreements --accept-source-agreements | Out-Null
    $clang = Find-InWingetPackages 'MartinStorsjo.LLVM-MinGW' 'x86_64-w64-mingw32-clang.exe'
    if (-not $clang) { throw 'LLVM-MinGW did not install. Try: winget install --id MartinStorsjo.LLVM-MinGW.UCRT -e' }
} else {
    Write-Done 'C compiler already installed'
}
# Only this session needs it: the finished reel.exe does not depend on it.
$env:Path = "$(Split-Path $clang -Parent);$env:Path"

Write-Step 'Building reel (a few minutes)'
$temp = $null
$source = if ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml'))) { $PSScriptRoot } else { $null }
try {
    if ($source) {
        Write-Done "using the source in $source"
    } else {
        $temp = Join-Path ([IO.Path]::GetTempPath()) "reel-install-$([guid]::NewGuid())"
        New-Item -ItemType Directory $temp | Out-Null
        $archive = Join-Path $temp 'reel.zip'
        Invoke-WebRequest $ArchiveUrl -OutFile $archive
        Expand-Archive $archive -DestinationPath $temp
        $source = (Get-ChildItem $temp -Directory | Select-Object -First 1).FullName
        Write-Done 'downloaded the source'
    }
    Push-Location $source
    try {
        & cargo "+$Toolchain" build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'the build failed; see the messages above' }
    } finally {
        Pop-Location
    }

    Write-Step 'Installing reel'
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    $target = Join-Path $InstallDir 'reel.exe'
    # Windows won't overwrite a running exe, but it can be moved aside.
    if (Test-Path $target) {
        Get-ChildItem $InstallDir -Filter 'reel.old.*.exe' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
        Move-Item $target (Join-Path $InstallDir "reel.old.$(Get-Date -Format yyyyMMddHHmmss).exe") -Force
    }
    Copy-Item (Join-Path $source 'target\release\reel.exe') $target -Force
    Get-ChildItem $InstallDir -Filter 'reel.old.*.exe' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    Add-ToUserPath $InstallDir
    Write-Done (& $target --version)
} finally {
    # The build output is several GB and is not needed once reel.exe is copied.
    if ($temp) { Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue }
}

# Tells other programs that PATH changed. Removing a variable that doesn't exist changes
# nothing else, but still sends the notification.
if ($script:PathChanged) { [Environment]::SetEnvironmentVariable('REEL_INSTALL_NOTIFY', $null, 'User') }

Write-Host "`nDone. Open a new terminal and run: reel" -ForegroundColor Green
Write-Host 'Use Windows Terminal for full-quality posters and video inside reel.'
