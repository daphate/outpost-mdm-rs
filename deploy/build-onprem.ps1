# Ф6: пайплайн per-customer on-prem сборки.
#
# По профилю заказчика собирает воспроизводимый bundle single-tenant инстанса:
#   1) release-бинари outpost-server + outpost-export (в WSL — Касперский
#      блокирует cargo на Windows, см. deploy.ps1);
#   2) срез данных заказчика из БД хаба в seed.db (outpost-export);
#   3) конфиги под домен заказчика: nginx (+ SSE-location), systemd, .env
#      с фиче-флагами из профиля.
#
# Локально, без CI (Docker/CI не используется — сборка на машине пользователя).
#
# Использование:
#   .\build-onprem.ps1 -ProfilePath .\onprem-profile.example.json -HubDb /root/outpost-mdm-rs/data/outpost.db -OutDir F:\onprem\example
#   .\build-onprem.ps1 -ProfilePath .\onprem-profile.example.json -OutDir F:\onprem\example -ConfigOnly
#
# -ConfigOnly пропускает WSL-сборку и срез — рендерит только конфиги (быстрая
# проверка шаблонов).

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ProfilePath,
    [string]$HubDb = '/root/outpost-mdm-rs/data/outpost.db',
    [Parameter(Mandatory = $true)][string]$OutDir,
    [string]$WslDistro = 'Ubuntu',
    [switch]$ConfigOnly
)

$ErrorActionPreference = 'Stop'

# --- профиль ------------------------------------------------------------
if (-not (Test-Path $ProfilePath)) { throw "профиль не найден: $ProfilePath" }
$profile = Get-Content -Raw $ProfilePath | ConvertFrom-Json

$customerId = [int]$profile.customer_id
$domain = [string]$profile.domain
if (-not $domain) { throw 'в профиле не задан domain' }
$secureCookies = if ($null -ne $profile.runtime.secure_cookies) { [bool]$profile.runtime.secure_cookies } else { $true }
$bindAddr = if ($profile.runtime.bind_addr) { [string]$profile.runtime.bind_addr } else { '127.0.0.1:8080' }
$dbPath = if ($profile.runtime.db_path) { [string]$profile.runtime.db_path } else { '/var/lib/outpost/outpost.db' }
$ballistics = [bool]$profile.feature_flags.ballistics
$bearingFed = [bool]$profile.feature_flags.bearing_federation

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Write-Host "==> профиль: заказчик $customerId ($($profile.customer_name)), домен $domain" -ForegroundColor Cyan

# --- 1. сборка бинарей в WSL -------------------------------------------
if (-not $ConfigOnly) {
    Write-Host '==> сборка release-бинарей в WSL (outpost-server, outpost-export)' -ForegroundColor Cyan
    $rsync = 'rsync -a --delete --exclude=target --exclude=.git /mnt/f/projects/outpost-mdm-rs/ /root/outpost-mdm-rs/'
    wsl -d $WslDistro -- bash -lc $rsync
    wsl -d $WslDistro -- bash -lc 'cd /root/outpost-mdm-rs && cargo build --release --bin outpost-server --bin outpost-export'
    wsl -d $WslDistro -- bash -lc "cp /root/outpost-mdm-rs/target/release/outpost-server /root/outpost-mdm-rs/target/release/outpost-export /mnt/$($OutDir -replace ':','' -replace '\\','/' )/" 2>$null
    Write-Host '    бинари в WSL: target/release/{outpost-server,outpost-export}'

    # --- 2. срез данных заказчика --------------------------------------
    Write-Host "==> срез данных заказчика $customerId → seed.db" -ForegroundColor Cyan
    $seedWsl = "/tmp/onprem_seed_$customerId.db"
    wsl -d $WslDistro -- bash -lc "rm -f $seedWsl; /root/outpost-mdm-rs/target/release/outpost-export '$HubDb' '$seedWsl' $customerId"
    $seedOut = Join-Path $OutDir 'seed.db'
    wsl -d $WslDistro -- bash -lc "cp $seedWsl '$($seedOut -replace ':','' -replace '\\','/' | ForEach-Object { '/mnt/' + $_.Substring(0,1).ToLower() + $_.Substring(1) })'" 2>$null
    Write-Host "    seed → $seedOut"
}

# --- 3. конфиги под домен ----------------------------------------------
Write-Host '==> рендер конфигов (nginx, systemd, .env)' -ForegroundColor Cyan

# APP_SECRET: сгенерировать 48 случайных байт (base64).
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$bytes = New-Object byte[] 48
$rng.GetBytes($bytes)
$appSecret = [Convert]::ToBase64String($bytes)

$envLines = @(
    "APP_SECRET=$appSecret",
    "BIND_ADDR=$bindAddr",
    "DB_PATH=$dbPath",
    "SECURE_COOKIES=$($secureCookies.ToString().ToLower())",
    "BALLISTICS_ENABLED=$($ballistics.ToString().ToLower())"
)
if ($bearingFed -and $profile.bearing.base_url) {
    $envLines += "BEARING_BASE_URL=$($profile.bearing.base_url)"
    $envLines += "BEARING_FED_TOKEN=$($profile.bearing.fed_token)"
}
$envPath = Join-Path $OutDir 'outpost.env'
Set-Content -Path $envPath -Value ($envLines -join "`n") -Encoding UTF8 -NoNewline

# systemd unit.
$service = @'
[Unit]
Description=Outpost MDM (on-prem, __DOMAIN__)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=outpost
Group=outpost
EnvironmentFile=/etc/outpost/outpost.env
ExecStart=/usr/local/bin/outpost-server
Restart=on-failure
RestartSec=3
StateDirectory=outpost
WorkingDirectory=/var/lib/outpost

[Install]
WantedBy=multi-user.target
'@ -replace '__DOMAIN__', $domain
Set-Content -Path (Join-Path $OutDir 'outpost-server.service') -Value $service -Encoding UTF8

# nginx: single-tenant, отдельный SSE-location для живой карты.
$nginx = @'
server {
    server_name __DOMAIN__;
    client_max_body_size 250M;
    add_header X-Content-Type-Options nosniff always;

    # Ф1: живой поток ситуационной карты (SSE) — без буферизации, длинный таймаут.
    location = /api/v1/live/stream {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 3600s;
    }

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Request-Id $request_id;
        proxy_http_version 1.1;
        proxy_read_timeout 120s;
        proxy_buffering off;
    }

    location = /healthz { access_log off; proxy_pass http://127.0.0.1:8080; }
    location = /readyz  { access_log off; proxy_pass http://127.0.0.1:8080; }

    listen 80;
    server_name __DOMAIN__;
    # TLS: выпустить сертификат `certbot --nginx -d __DOMAIN__` (переведёт на 443).
}
'@ -replace '__DOMAIN__', $domain
Set-Content -Path (Join-Path $OutDir "nginx-$domain.conf") -Value $nginx -Encoding UTF8

# README с инструкцией развёртывания.
$readme = @'
On-prem bundle для __DOMAIN__ (заказчик __CUSTOMER__).

Состав:
  outpost-server           release-бинарь сервера (если сборка не -ConfigOnly)
  outpost-export           инструмент среза (для повторных выгрузок)
  seed.db                  срез данных заказчика (single-tenant)
  outpost.env              переменные окружения (APP_SECRET уже сгенерирован)
  outpost-server.service   systemd unit
  nginx-__DOMAIN__.conf    конфиг nginx (SSE-location включён)

Развёртывание на чистом хосте:
  1. useradd -r -s /usr/sbin/nologin outpost
  2. install -m755 outpost-server /usr/local/bin/
     install -m755 outpost-export /usr/local/bin/
  3. mkdir -p /var/lib/outpost && cp seed.db /var/lib/outpost/outpost.db
     chown -R outpost:outpost /var/lib/outpost
  4. mkdir -p /etc/outpost && cp outpost.env /etc/outpost/  (chmod 600)
  5. cp outpost-server.service /etc/systemd/system/
     systemctl daemon-reload && systemctl enable --now outpost-server
  6. cp nginx-__DOMAIN__.conf /etc/nginx/sites-available/ && ln -s ... sites-enabled/
     certbot --nginx -d __DOMAIN__
  7. Проверка: curl https://__DOMAIN__/healthz

Приложения-клиенты указывают на https://__DOMAIN__ через enrollment-QR
(серверный URL data-driven — пересборка APK под URL не требуется). Брендирование
и baked-in allowed-classes под заказчика — через Android product flavors
(точка расширения, см. docs/ON-PREM-BUILD.md).
'@ -replace '__DOMAIN__', $domain -replace '__CUSTOMER__', "$customerId"
Set-Content -Path (Join-Path $OutDir 'README.txt') -Value $readme -Encoding UTF8

Write-Host "==> готово: bundle в $OutDir" -ForegroundColor Green
Get-ChildItem $OutDir | Select-Object Name, Length | Format-Table -AutoSize
