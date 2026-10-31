@echo off
setlocal EnableExtensions
title EXMACHINA Installer

:: Bootstrap shell: the real logic lives in the PowerShell script written by :write_ps1 (PS 5.1 compatible).
:: Usage: install.bat [--bin^|--src] [--port N]   |   --help shows usage (fully local, no network).
:: NOTE: this file is intentionally pure ASCII (no chcp / no CJK here) so cmd's batch
::       parser never hits the codepage 65001 multi-byte line-offset bug.

set "MODE="
set "PORT=4173"
set "ARGSOK=1"

:parse
if "%~1"=="" goto :parsed
if /i "%~1"=="--bin" ( set "MODE=bin" & shift & goto :parse )
if /i "%~1"=="--src" ( set "MODE=src" & shift & goto :parse )
if /i "%~1"=="--help" goto :help
if /i "%~1"=="-h" goto :help
if /i "%~1"=="--port" (
  if "%~2"=="" (
    echo [X] --port requires a port number.
    exit /b 1
  )
  set "PORT=%~2"
  shift
  shift
  goto :parse
)
echo [X] unknown argument: %~1
goto :help

:parsed
where powershell >nul 2>&1
if errorlevel 1 (
  echo [X] Windows PowerShell not found. It ships with Windows; repair your environment and retry.
  exit /b 1
)

set "PS1=%TEMP%\exm_install.ps1"
call :write_ps1
:: PS 5.1 parses BOM-less files as ANSI: rewrite the generated UTF-8 script with a BOM.
powershell -NoProfile -Command "$c = Get-Content -Raw -Encoding UTF8 '%PS1%'; [System.IO.File]::WriteAllText('%PS1%', $c, (New-Object System.Text.UTF8Encoding $true))"
powershell -NoProfile -ExecutionPolicy Bypass -File "%PS1%" -Mode "%MODE%" -Port %PORT%
set "RC=%errorlevel%"
del "%PS1%" >nul 2>&1
exit /b %RC%

:write_ps1
>> "%PS1%" echo param([string]$Mode = '', [int]$Port = 4173)
>> "%PS1%" echo $ErrorActionPreference = 'Stop'
>> "%PS1%" echo $repo = 'ExMachina-Project/ExMachina'
>> "%PS1%" echo $srcDir = 'ExMachina'
>> "%PS1%" echo $binDir = 'exmachina'
>> "%PS1%" echo $branch = 'main'
>> "%PS1%" echo $tag = $env:EXM_VERSION
>> "%PS1%" echo if ($env:EXM_BRANCH) { $branch = $env:EXM_BRANCH }
>> "%PS1%" echo if ($env:EXM_DIR) { $srcDir = $env:EXM_DIR }
>> "%PS1%" echo $t0 = Get-Date
>> "%PS1%" echo function Ok($m) { Write-Host '  [OK] ' -ForegroundColor Green -NoNewline; Write-Host $m }
>> "%PS1%" echo function Info($m) { Write-Host '  ..   ' -ForegroundColor Cyan -NoNewline; Write-Host $m }
>> "%PS1%" echo function Warn($m) { Write-Host '  [!]  ' -ForegroundColor Yellow -NoNewline; Write-Host $m }
>> "%PS1%" echo function Fail($m, $hint) { Write-Host "  [X] $m" -ForegroundColor Red; if ($hint) { Write-Host "      Hint: $hint" -ForegroundColor Yellow }; exit 1 }
>> "%PS1%" echo function Assert-Port($p) {
>> "%PS1%" echo   $busy = Get-NetTCPConnection -LocalPort $p -State Listen -ErrorAction SilentlyContinue
>> "%PS1%" echo   if ($busy) { Fail "port $p is already in use" 'pick another port: install.bat --port 8173, or stop the process holding it' }
>> "%PS1%" echo }
>> "%PS1%" echo try {
>> "%PS1%" echo   Write-Host ''
>> "%PS1%" echo   Write-Host '  EX.MACHINA - agent cluster installer' -ForegroundColor Cyan
>> "%PS1%" echo   if ($Mode -ne 'bin' -and $Mode -ne 'src') {
>> "%PS1%" echo     Write-Host '  Pick an install method:' -ForegroundColor Cyan
>> "%PS1%" echo     '  1) prebuilt release   download and run, no Rust / Node.js needed (recommended)' | Write-Host -ForegroundColor Green
>> "%PS1%" echo     '  2) build from source  bootstrap toolchains, clone, build (any platform, ~3-5 min)' | Write-Host -ForegroundColor Yellow
>> "%PS1%" echo     '  Enter = 1' | Write-Host -ForegroundColor DarkGray
>> "%PS1%" echo     $reply = Read-Host '  ^'
>> "%PS1%" echo     if ($reply -eq '2' -or $reply -eq 'src') { $Mode = 'src' } else { $Mode = 'bin' }
>> "%PS1%" echo   }
>> "%PS1%" echo   if ($Mode -eq 'bin') {
>> "%PS1%" echo   Write-Host '  [1/4] Environment preflight (prebuilt release)...'
>> "%PS1%" echo   $drive = (Get-Location).Path.Substring(0,1)
>> "%PS1%" echo   $freeMB = [math]::Round((Get-PSDrive $drive).Free / 1MB)
>> "%PS1%" echo   if ($freeMB -lt 512) { Fail "not enough free disk space (${freeMB}MB, need 512MB)" 'free up space or switch to another working directory' }
>> "%PS1%" echo   Ok "${freeMB}MB free"
>> "%PS1%" echo   Write-Host '  [2/4] Resolving version and platform...'
>> "%PS1%" echo   $plat = 'windows-x64'
>> "%PS1%" echo   $file = "exmachina-$plat.zip"
>> "%PS1%" echo   if (-not $tag) {
>> "%PS1%" echo     try {
>> "%PS1%" echo       $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
>> "%PS1%" echo       $tag = $rel.tag_name
>> "%PS1%" echo     } catch { $tag = $null }
>> "%PS1%" echo   }
>> "%PS1%" echo   if (-not $tag) { Fail 'cannot resolve the latest release tag' 'network restricted or GitHub API rate-limited; set EXM_VERSION=v1.2.0 and retry' }
>> "%PS1%" echo   if ($tag -notlike 'v*') { $tag = "v$tag" }
>> "%PS1%" echo   Ok "version $tag on $plat"
>> "%PS1%" echo   Write-Host "  [3/4] Downloading $file..."
>> "%PS1%" echo   $url = "https://github.com/$repo/releases/download/$tag/$file"
>> "%PS1%" echo   $tmp = Join-Path $env:TEMP "exm-install-$PID"
>> "%PS1%" echo   $null = New-Item -ItemType Directory -Path $tmp -Force
>> "%PS1%" echo   Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile "$tmp\$file"
>> "%PS1%" echo   try {
>> "%PS1%" echo     Invoke-WebRequest -UseBasicParsing -Uri "$url.sha256" -OutFile "$tmp\$file.sha256"
>> "%PS1%" echo     $want = (Get-Content "$tmp\$file.sha256" -TotalCount 1).Split(' ')[0]
>> "%PS1%" echo     $have = (Get-FileHash -Algorithm SHA256 "$tmp\$file").Hash
>> "%PS1%" echo     if ($want -ne $have) { Fail 'checksum mismatch' 'the download may be corrupted or tampered; clear the temp dir and retry' }
>> "%PS1%" echo     Ok 'checksum matches'
>> "%PS1%" echo   } catch {
>> "%PS1%" echo     Warn 'release has no .sha256 sidecar, skipping checksum'
>> "%PS1%" echo   }
>> "%PS1%" echo   Write-Host "  [4/4] Extract and install (dir: $binDir)..."
>> "%PS1%" echo   if (Test-Path $binDir) { Info 'existing install detected - upgrading (program files replaced, .exmachina data preserved)' }
>> "%PS1%" echo   $null = New-Item -ItemType Directory -Path $binDir -Force
>> "%PS1%" echo   $stage = Join-Path $binDir ".stage-$PID"
>> "%PS1%" echo   Expand-Archive -Path "$tmp\$file" -DestinationPath $stage -Force
>> "%PS1%" echo   Copy-Item -Path "$stage\*" -Destination $binDir -Recurse -Force
>> "%PS1%" echo   Remove-Item $stage -Recurse -Force
>> "%PS1%" echo   Remove-Item $tmp -Recurse -Force
>> "%PS1%" echo   Assert-Port $Port
>> "%PS1%" echo   $elapsed = [math]::Round(((Get-Date) - $t0).TotalSeconds)
>> "%PS1%" echo   Write-Host ''
>> "%PS1%" echo   Write-Host '  ==============================================' -ForegroundColor Green
>> "%PS1%" echo   Write-Host '  [OK] Ready!' -ForegroundColor Green
>> "%PS1%" echo   Write-Host "     version   $tag ($plat)"
>> "%PS1%" echo   Write-Host "     dir       $((Get-Location).Path)\$binDir"
>> "%PS1%" echo   Write-Host "     port      $Port    elapsed   ${elapsed}s"
>> "%PS1%" echo   Write-Host '  ==============================================' -ForegroundColor Green
>> "%PS1%" echo   Write-Host ''
>> "%PS1%" echo   Write-Host "  Open  http://127.0.0.1:$Port  in your browser."
>> "%PS1%" echo   Write-Host '  First run: open the Models page, add a profile, pick a vendor, paste your API key, set it as default.'
>> "%PS1%" echo   Write-Host '  Stop: Ctrl+C. Next time: cd exmachina, then exm-gateway.exe'
>> "%PS1%" echo   $env:EXM_PORT = "$Port"
>> "%PS1%" echo   Set-Location $binDir
>> "%PS1%" echo   & ".\exm-gateway.exe"
>> "%PS1%" echo   exit $LASTEXITCODE
>> "%PS1%" echo   }
>> "%PS1%" echo   Write-Host '  [1/7] Environment preflight (from source)...'
>> "%PS1%" echo   foreach ($tool in @('git','curl','tar')) {
>> "%PS1%" echo     if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { Fail "missing dependency: $tool" 'install it and make sure it is on PATH' }
>> "%PS1%" echo   }
>> "%PS1%" echo   $drive = (Get-Location).Path.Substring(0,1)
>> "%PS1%" echo   $freeGB = [math]::Round((Get-PSDrive $drive).Free / 1GB, 1)
>> "%PS1%" echo   if ($freeGB -lt 3) { Fail "not enough free disk space (${freeGB}GB, need 3GB)" 'free up space or switch to another working directory' }
>> "%PS1%" echo   Ok "dependencies present, ${freeGB}GB free"
>> "%PS1%" echo   Write-Host '  [2/7] Rust toolchain...'
>> "%PS1%" echo   if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
>> "%PS1%" echo     Info 'cargo not found, installing rustup (stable)...'
>> "%PS1%" echo     $rustInit = "$env:TEMP\rustup-init.exe"
>> "%PS1%" echo     Invoke-WebRequest -UseBasicParsing -Uri 'https://win.rustup.rs/x86_64' -OutFile $rustInit
>> "%PS1%" echo     Start-Process -FilePath $rustInit -ArgumentList '-y','--default-toolchain','stable' -Wait
>> "%PS1%" echo     Remove-Item $rustInit -Force
>> "%PS1%" echo     $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
>> "%PS1%" echo   }
>> "%PS1%" echo   $rv = cargo --version
>> "%PS1%" echo   if ($LASTEXITCODE -ne 0) { Fail 'cargo is not usable' 'rustup install may be incomplete; reopen a terminal and retry' }
>> "%PS1%" echo   Ok "Rust $rv"
>> "%PS1%" echo   Write-Host '  [3/7] Node.js (22 or newer)...'
>> "%PS1%" echo   $needNode = $true
>> "%PS1%" echo   if (Get-Command node -ErrorAction SilentlyContinue) {
>> "%PS1%" echo     $major = [int](node -p 'process.versions.node.split(".")[0]')
>> "%PS1%" echo     if ($major -ge 22) { $needNode = $false }
>> "%PS1%" echo   }
>> "%PS1%" echo   if ($needNode) {
>> "%PS1%" echo     Write-Host '  [X] Node.js 22+ is required.' -ForegroundColor Red
>> "%PS1%" echo     Write-Host '      Install it, then reopen a terminal and run this script again.' -ForegroundColor Yellow
>> "%PS1%" echo     Start-Process 'https://nodejs.org'
>> "%PS1%" echo     exit 1
>> "%PS1%" echo   }
>> "%PS1%" echo   Ok "Node.js $(node --version)"
>> "%PS1%" echo   Write-Host "  [4/7] Fetching sources (branch $branch, dir $srcDir)..."
>> "%PS1%" echo   if (Test-Path "$srcDir\.git") {
>> "%PS1%" echo     Info 'directory exists, pulling latest...'
>> "%PS1%" echo     git -C $srcDir pull --rebase
>> "%PS1%" echo     if ($LASTEXITCODE -ne 0) { Fail 'git pull failed' 'check the network, or resolve local changes and retry' }
>> "%PS1%" echo   } else {
>> "%PS1%" echo     git clone --depth 1 -b $branch "https://github.com/$repo.git" $srcDir
>> "%PS1%" echo     if ($LASTEXITCODE -ne 0) { Fail 'git clone failed' 'check the network and the branch name (EXM_BRANCH, default main)' }
>> "%PS1%" echo   }
>> "%PS1%" echo   Set-Location $srcDir
>> "%PS1%" echo   Ok "sources ready at $(git rev-parse --short HEAD)"
>> "%PS1%" echo   Write-Host '  [5/7] Building Rust (first run takes about 3-5 minutes, streaming output)...'
>> "%PS1%" echo   cargo build --release -p exm-gateway -p exm-cli
>> "%PS1%" echo   if ($LASTEXITCODE -ne 0) { Fail 'Rust build failed' 'the full error is printed above; fix and run this script again' }
>> "%PS1%" echo   Ok 'Rust build finished'
>> "%PS1%" echo   Write-Host '  [6/7] Building WebUI (npm ci + build:webui)...'
>> "%PS1%" echo   npm ci --no-audit --no-fund
>> "%PS1%" echo   if ($LASTEXITCODE -ne 0) { Fail 'npm ci failed' 'check the Node version and npm registry; try npm config set registry https://registry.npmmirror.com' }
>> "%PS1%" echo   npm run build:webui
>> "%PS1%" echo   if ($LASTEXITCODE -ne 0) { Fail 'WebUI build failed' 'the full error is printed above; fix and run this script again' }
>> "%PS1%" echo   Ok 'WebUI build finished'
>> "%PS1%" echo   Write-Host "  [7/7] Starting gateway (port $Port)..."
>> "%PS1%" echo   Assert-Port $Port
>> "%PS1%" echo   Write-Host ''
>> "%PS1%" echo   Write-Host '  ==============================================' -ForegroundColor Green
>> "%PS1%" echo   Write-Host '  [OK] Install complete!' -ForegroundColor Green
>> "%PS1%" echo   $elapsed = [math]::Round(((Get-Date) - $t0).TotalSeconds)
>> "%PS1%" echo   Write-Host "     dir    $((Get-Location).Path)"
>> "%PS1%" echo   Write-Host "     port   $Port    elapsed   ${elapsed}s"
>> "%PS1%" echo   Write-Host '  ==============================================' -ForegroundColor Green
>> "%PS1%" echo   Write-Host ''
>> "%PS1%" echo   Write-Host "  Open  http://127.0.0.1:$Port  in your browser."
>> "%PS1%" echo   Write-Host '  First run: open the Models page, add a profile, pick a vendor, paste your API key, set it as default.'
>> "%PS1%" echo   Write-Host '  Terminal chat: cargo run --release -p exm-cli -- chat "hello"'
>> "%PS1%" echo   Write-Host '  Stop: Ctrl+C. Next time: cd into the install dir and run cargo run --release -p exm-gateway'
>> "%PS1%" echo   $env:EXM_PORT = "$Port"
>> "%PS1%" echo   cargo run --release -p exm-gateway
>> "%PS1%" echo } catch {
>> "%PS1%" echo   Write-Host "  [X] unexpected failure: $_" -ForegroundColor Red
>> "%PS1%" echo   exit 1
>> "%PS1%" echo }
goto :eof

:help
echo EXMACHINA installer (Windows: cmd bootstrap + PowerShell runner)
echo.
echo Usage: install.bat [--bin^|--src] [--port N]
echo.
echo Options:
echo   --bin       prebuilt release (skip the prompt; download and run, no toolchains)
echo   --src       build from source (skip the prompt: toolchains + clone + build)
echo   --port N    gateway listen port (default 4173)
echo   --help      show this help (fully local, no network)
echo.
echo Without --bin/--src the installer asks interactively (default: prebuilt).
echo.
echo Environment variables (read by the PowerShell section):
echo   EXM_VERSION  release tag for --bin (default: latest)
echo   EXM_BRANCH   branch to clone for --src (default main)
echo   EXM_DIR      source install directory for --src (default .\ExMachina)
echo   EXM_PORT     gateway listen port (--port overrides)
echo.
echo Notes: source builds need 3GB+ free disk, first Rust build takes about 3-5
echo        minutes; when done, open http://127.0.0.1:4173 and configure your API
echo        key on the Models page.
exit /b 0
