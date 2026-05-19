# Build + deploy outpost-server to mdm.secondf8n.tech.
#
# Build environment: WSL2 Ubuntu 24.04 (matches the production host's
# glibc 2.39 exactly — no cross-compile, no musl gymnastics, no docker).
# The `wsl` invocation hops into the WSL distro to run `cargo build`,
# then the resulting Linux ELF is `scp`'d onto the host.
#
# Why WSL and not native Windows + cargo-zigbuild? Kaspersky's heuristic
# engine intercepts cargo's freshly-built linker temp files on this
# workstation, deadlocking the build pipeline. WSL's separate filesystem
# is not scanned, so the build completes cleanly.
#
# Why not the docker image any more? See deploy/README and ../docs/DEPLOY.md.
#
# Usage:
#   .\deploy.ps1
#   .\deploy.ps1 -RemoteHost mdm.secondf8n.tech -SshKey "$env:USERPROFILE\.ssh\awscalifornia"

[CmdletBinding()]
param(
    [string]$RemoteHost = 'mdm.secondf8n.tech',
    [string]$SshUser = 'ubuntu',
    [string]$SshKey = "$env:USERPROFILE\.ssh\awscalifornia",
    [string]$WslDistro = 'Ubuntu',
    [int]$KeepRevisions = 3
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

Write-Host '==> resolving git sha' -ForegroundColor Cyan
$sha = (git rev-parse --short HEAD).Trim()
if (-not $sha) { throw 'git rev-parse failed' }
Write-Host "    sha=$sha"

Write-Host '==> rsync source into WSL ~/outpost-mdm-rs' -ForegroundColor Cyan
$rsyncCmd = "rsync -a --delete --exclude=target --exclude=.git --exclude=.tmp /mnt/f/projects/outpost-mdm-rs/ /root/outpost-mdm-rs/"
wsl -d $WslDistro -- bash -lc $rsyncCmd
if ($LASTEXITCODE -ne 0) { throw 'rsync into WSL failed' }

Write-Host '==> cargo build --release (in WSL)' -ForegroundColor Cyan
# v0.18.14: unset HTTP(S)_PROXY перед cargo. WSL2 auto-mirror подтягивает
# Windows-side HTTP_PROXY (xray от Hide.My.Name VPN на 127.0.0.1:1301),
# и если xray временно дропнут — cargo не может скачать crates с
# crates.io. Прокси нам не нужен для crates.io в любом случае.
$buildCmd = 'unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy; . $HOME/.cargo/env && cd /root/outpost-mdm-rs && cargo build --release --bin outpost-server'
wsl -d $WslDistro -- bash -lc $buildCmd
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed inside WSL' }

# v0.18.11: gate deploy on the unit-test suite. Catches regressions
# before they hit prod (e.g. v0.18.7 byte-slicing panic, v0.18.9 TZ
# parsing). Release-profile tests compile slowly first time but reuse
# the same target/release artifacts from the build above on subsequent
# runs, so the typical overhead is ~5 seconds.
Write-Host '==> cargo test --release (in WSL)' -ForegroundColor Cyan
$testCmd = 'unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy; . $HOME/.cargo/env && cd /root/outpost-mdm-rs && cargo test --release --lib -p outpost-server --quiet'
wsl -d $WslDistro -- bash -lc $testCmd
if ($LASTEXITCODE -ne 0) { throw 'cargo test failed inside WSL — fix tests before deploying' }

Write-Host '==> staging binary to .tmp/' -ForegroundColor Cyan
$tmpBin = "$repoRoot\.tmp\outpost-server.new"
wsl -d $WslDistro -- cp /root/outpost-mdm-rs/target/release/outpost-server /mnt/f/projects/outpost-mdm-rs/.tmp/outpost-server.new
if (-not (Test-Path $tmpBin)) { throw "binary not at $tmpBin" }
$sz = (Get-Item $tmpBin).Length
Write-Host "    binary: $tmpBin ($([math]::Round($sz/1MB,1)) MB)"

Write-Host "==> scp -> ${RemoteHost}:/tmp" -ForegroundColor Cyan
& scp -i "$SshKey" -o StrictHostKeyChecking=no $tmpBin "${SshUser}@${RemoteHost}:/tmp/outpost-server.new"
if ($LASTEXITCODE -ne 0) { throw 'scp failed' }

$installScript = @"
set -e
sudo install -m 0755 -o root -g root /tmp/outpost-server.new /usr/local/bin/outpost-server.$sha
rm -f /tmp/outpost-server.new
sudo ln -sfn /usr/local/bin/outpost-server.$sha /usr/local/bin/outpost-server
sudo systemctl restart outpost-server
sleep 2
sudo systemctl --no-pager --lines=5 status outpost-server | head -12
ls -t /usr/local/bin/outpost-server.* 2>/dev/null | tail -n +$($KeepRevisions + 1) | xargs -r sudo rm -v
"@

Write-Host "==> install + restart on $RemoteHost" -ForegroundColor Cyan
& ssh -i "$SshKey" -o StrictHostKeyChecking=no "${SshUser}@${RemoteHost}" $installScript
if ($LASTEXITCODE -ne 0) { throw 'remote install failed' }

Write-Host "==> waiting for https://$RemoteHost/healthz" -ForegroundColor Cyan
$deadline = (Get-Date).AddSeconds(20)
while ((Get-Date) -lt $deadline) {
    try {
        $r = Invoke-WebRequest -Uri "https://$RemoteHost/healthz" -TimeoutSec 3 -UseBasicParsing
        if ($r.StatusCode -eq 200) {
            Write-Host "    healthy: $($r.Content)" -ForegroundColor Green
            exit 0
        }
    } catch { Start-Sleep -Seconds 1 }
}
throw "healthz did not return 200 within 20s"
