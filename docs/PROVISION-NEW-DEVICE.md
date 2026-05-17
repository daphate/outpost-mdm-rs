# Провижининг нового Ulefone из коробки — оператору в поле

**Дата:** 2026-05-18 · **MDM-server:** `mdm.secondf8n.tech` ≥ 0.16.0 · **Outpost-Android:** rc42 b37+

Сквозной сценарий для оператора, у которого на руках новый Ulefone Armor 28 Ultra (или другой Ulefone/Doogee/Oukitel/Realme с MTK chip + DuraSpeed) и админ-доступ к MDM Web UI. Цель: за ≤10 минут довести устройство от «распакован из коробки» до «зачислен в MDM, телеметрия идёт в Grafana, авто-обновления работают». **Без adb, без рабочей машины, без developer mode.**

## Предусловия

| Что | Где / как |
|---|---|
| Доступ к MDM Web UI | `https://mdm.secondf8n.tech` с логином admin и паролем |
| Версия MDM-сервера | `/healthz` → `{"version":"0.16.0"}` или выше |
| Cloud.ru creds на сервере | `CLOUDRU_TENANT_ID/KEY_ID/SECRET` заданы в `/etc/outpost/env`; в логах startup'а должна быть строка `Cloud.ru presigner enabled` (`sudo journalctl -u outpost-server -n 20 \| grep Cloud`) |
| APK в Cloud.ru | объект `apks/latest/app-debug.apk` доступен в bucket'е `outpost` (verified: HEAD/GET с presigned URL возвращает 200/206, MIME `application/vnd.android.package-archive`) |
| На устройстве | заряд ≥ 30 %, Wi-Fi с доступом в интернет |

Если MDM-сервер на 0.15 или ниже — APK-QR блок не появится на странице enrollment, нужно сначала задеплоить ≥ 0.16.

## Шаг 1 — создать устройство в MDM (≤1 минута)

1. Открыть `https://mdm.secondf8n.tech` → логин admin'а.
2. **Устройства** → **Новое**.
3. Ввести:
   - **Серийный №** — короткий уникальный человекочитаемый ID (`RUGGED-04`, `DEMO-MO-12`). Это то имя, по которому ты будешь искать устройство в Grafana drill-down и в Web UI.
   - **Display name** — имя владельца или назначение (`Иванов, СОЦ`, `тестовый стенд лаб 3`).
4. Сохранить. Откроется карточка устройства.
5. На карточке → кнопка **«Сгенерировать enrollment payload»**. Это создаст одноразовый секрет (`enrollment_secret`) и перебросит на страницу `/devices/{id}/enroll`.

## Шаг 2 — оператор показывает APK-QR (≤30 секунд)

На странице `/devices/{id}/enroll` будут **два QR'а**, в правильной последовательности:

- **Шаг 1 — установка приложения** (новый блок v0.16, синий border). Содержит:
  - QR со SigV4 presigned URL на `apks/latest/app-debug.apk` в Cloud.ru. Действителен 7 дней.
  - Прямую ссылку в моноспейс (для копи-паста / Telegram, если QR не сканируется).
  - Метку «QR действителен до DD.MM.YYYY HH:MM UTC».

- **Шаг 2 — полезная нагрузка регистрации** (существующий блок, янтарный фон). Содержит:
  - Enrollment QR (`outpost-mdm://v1/<base64url(JSON)>`).
  - Сам JSON в моноспейс для manual-paste fallback.
  - Кнопку «Скачать enrollment.json» для оффлайн-bootstrap через флешку.

Дай телефон в руки конечному пользователю и попроси:

1. Открыть **штатную камеру Android** (Outpost ещё не установлен).
2. Навести на **первый QR** (APK). Камера распознает URL — тапнуть всплывающую подсказку → откроется браузер.
3. Браузер скачает `app-debug.apk` (≈170 МБ, по сотовому займёт минуту-две, по Wi-Fi секунд тридцать).
4. После скачивания тапнуть «Открыть» → Android спросит «Установить из неизвестного источника?» → выдать разрешение источнику (Chrome / Files / Telegram) → «Установить».

## Шаг 3 — DisclaimerScreen + OnboardingScreen (≤3 минуты)

После установки на экране появится иконка **Outpost** (label «Штаб»). Открыть.

1. **DisclaimerScreen** — пользователь читает правила, тапает «Принять».
2. **OnboardingScreen** (rc42 b38+) — wizard проводит через permission'ы которые Android не выдаёт runtime'ом. Каждый шаг открывает соответствующий раздел системных Настроек, юзер делает один тап:
   - **«Доступ ко всем файлам»** (MANAGE_EXTERNAL_STORAGE) — Settings → Outpost → toggle ON.
   - **«Не оптимизировать батарею»** (IGNORE_BATTERY_OPTIMIZATIONS) — system dialog, «Разрешить».
   - **«Устанавливать неизвестные приложения»** (REQUEST_INSTALL_PACKAGES) — Settings → Outpost → toggle ON. Нужно для MDM self-update.
   - **Camera / Microphone / Fine-Location** — обычные runtime-диалоги.
   - **DuraSpeed (MTK only)** — wizard детектирует `com.mediatek.duraspeed` в системе, открывает Settings → DuraSpeed. **Найти «Штаб» в списке, включить для него toggle** (это per-app whitelist, см. INSIGHT-048 §UPDATE 2026-05-18). Глобальный switch DuraSpeed оставить **включённым** — для остальных приложений экономия батареи продолжит работать.

Каждый шаг можно пропустить. Wizard сохраняет прогресс через `ModelPreferences.onboardingComplete=true` и больше не показывается на следующих startup'ах. Перезапустить — через Settings → «О приложении» → «Открыть мастер настройки».

После завершения wizard'а откроется Home.

## Шаг 4 — оператор показывает enrollment QR (≤1 минута)

Теперь Outpost установлен и сконфигурирован, осталось зачислить его в MDM.

1. На телефоне: ⚙ **Settings** → секция **Телеметрия** → кнопка **«Подключить по QR»**. Откроется камера-сканер.
2. Навести на **второй QR** на странице MDM (Шаг 2 — enrollment payload).
3. На устройстве появится экран **Confirm**:
   - server_url: `https://mdm.secondf8n.tech`
   - device_id: `<N>`
   - serial: `RUGGED-04`
   - secret: `xxxxxxxx…`
4. Тапнуть **«Подключить →»**.
5. Через 1-2 секунды появится **«● Подключено к MDM»** с device_id и сроком действия токена (90 дней).

`telemetryEndpoint`, `telemetryToken`, `telemetryEnabled=true` выставляются автоматически. Через ≤30 секунд в Settings → Телеметрия появится «Последняя загрузка: OK (… events → OTLP /v1/logs, HTTP 200)».

## Шаг 5 — verify в Web UI (≤30 секунд)

На admin машине:

1. `https://mdm.secondf8n.tech` → **Устройства** → найти `RUGGED-04`.
2. В карточке устройства должно быть:
   - **is_enrolled: true**
   - **last_seen_at: <в пределах минуты>**
   - **app_version: 1.0.0-rc42-bNN**
   - **app_version_code: NN**
3. В **Grafana** (через Tailscale, `http://mdm-secondf8n:3000`) → Dashboards → Outpost → **Device drill-down** → выбрать `RUGGED-04` в `$device` dropdown → видим battery%, RAM, last_seen.

## Если что-то пошло не так

### APK QR не сканируется

- Проверь что страница `/devices/{id}/enroll` действительно показывает блок «Шаг 1» с QR'ом. Если блок отсутствует — MDM-сервер собран без CLOUDRU_* env'ов, нужна доконфигурация на сервере (см. `docs/DEPLOY.md` §«Cloud.ru read-only IAM creds»).
- QR действителен 7 дней. Если давно (например, оператор открыл страницу неделю назад и сейчас пытается дать) — обнови страницу через F5 / Ctrl+R, она сгенерирует свежий URL.
- На некоторых рабочих Android-камерах распознавание QR-кодов отключено по умолчанию. Включить в `Camera → Settings → Scan QR codes`, либо использовать любой QR-сканер из стора (Google Lens работает без интернета на узнавание QR).

### APK скачался, но «Установка из неизвестных источников» не предлагает

Источник, через который пришёл APK, не имеет permission на install. Открыть `Settings → Apps → Special access → Install unknown apps → <твой браузер>` → toggle ON, потом вернуться в Downloads → тапнуть APK → согласиться.

### Outpost установлен, но Onboarding не показывает DuraSpeed-шаг

Проверь что устройство действительно MTK. На Android:
- Снять статус: `Settings → About phone → Hardware info` (или аналогичный пункт). Должен показывать `MediaTek Dimensity 9300+` или подобное.
- Если устройство не MTK (например, Snapdragon / Tensor) — wizard корректно скрывает шаг, делать ничего не надо.
- Если MTK, но шаг не показан — возможно пакет `com.mediatek.duraspeed` отсутствует на этой прошивке (Ulefone иногда выпускает варианты без него). Через adb или другое устройство можно verify: `pm list packages | grep duraspeed`.

### Подключение по QR падает с «HTTP 401 — секрет уже использован»

Enrollment secret одноразовый. Если кто-то уже отсканировал этот же QR раньше — secret израсходован. Решение: на странице `/devices/{id}/enroll` → кнопка «Перегенерировать (текущий станет недействителен)» → откроется новая страница со свежим QR.

### Settings → Телеметрия → «HTTP 401 (token missing or invalid)» через несколько минут после подключения

Возможные причины:
- Admin удалил устройство из MDM (`DELETE /api/v1/devices/{id}`) → device session revoked. Нужно создать новое device, сделать новый enrollment.
- Admin отправил команду `revoke-enrollment` (`POST /api/v1/devices/{id}/revoke-enrollment`) → client wiped token. Аналогично, заново enroll.

### Заряд QR на странице — `Шаг 1: QR действителен до …` уже в прошлом

Просто обнови страницу — presigned URL генерируется на каждый GET страницы заново, TTL отсчитывается от момента генерации.

## Что MDM делает после Шага 5 автоматически (без участия оператора)

- **Telemetry**: каждые 30 минут `POST /api/v1/sync` с heartbeat + applied_commands + current_state.
- **OTLP**: events отправляются в `/v1/logs` / `/v1/metrics` / `/v1/traces` → Grafana drill-down показывает их в течение минуты.
- **APK auto-discovery**: каждые 15 минут MDM хитит upstream `apks/latest/version.txt` → видит новые сборки → пишет в `application_versions` с бейджем 🟡 discovery.
- **APK auto-update**: если admin создал `application_rollouts` (canary или fleet), устройство на /sync узнает `update_available` → `ApkUpdater` скачает APK, проверит SHA256, запустит `PackageInstaller.Session.commit()` → юзер увидит confirm-dialog «Установить новую версию Outpost?». На rc42 b38+ при выданном `REQUEST_INSTALL_PACKAGES` permission'е это происходит автоматически.
- **Per-device настройки**: admin может через Web UI отправлять `update-config` команды (TTS режим, preferred LLM, log level и т.д.) — устройство применит на ближайшем /sync.

## Известные ограничения сейчас (что в roadmap)

- **Один shared read-only Cloud.ru ключ на весь fleet**. Per-device персонализированные creds — следующий этап (когда сделаем — на каждом enrollment'е будем выдавать ключ scoped только на нужные объекты, leak одного ключа не компрометирует остальные device'ы).
- **APK source — публичный bucket с фиксированными creds**, без anti-replay. Если злоумышленник перехватит presigned URL в течение 7 дней, он сможет скачать APK. Сам APK — open distribution, никакого секрета в этом нет, но контракт «только наши устройства качают» не enforced.
- **Wizard не auto-redirect'ит в Settings**. Каждый permission-step требует тапа «Открыть Настройки» → пользователь сам тапает toggle → возвращается. Полностью silent install / config возможен только через Device-Owner provisioning (NFC/QR factory-reset enrollment), которое в roadmap.
- **Onboarding wizard на Snapdragon/Tensor пропускает DuraSpeed-шаг автоматически**, но если на каком-то OEM-скине есть аналогичный proprietary task-killer (например MIUI `App auto-start`, Huawei `Protected apps`) — он не детектится. Юзер сам должен внести «Штаб» в whitelist соответствующего сервиса. См. [INSIGHT-048](https://github.com/daphate/tactical-ar-hud/tree/master/tools) в tactical-ar-hud для актуального списка.

## Связанные документы

- [`docs/DEPLOY.md`](DEPLOY.md) — как настроить mdm-сервер с нуля, env-vars, systemd, nginx.
- [`prototypes/outpost-android/PROVISIONING.md`](https://github.com/daphate/tactical-ar-hud/blob/master/prototypes/outpost-android/PROVISIONING.md) (AR Hud repo) — полный сценарий с дополнительными вариантами (offline через `enrollment.json` на флешку, bulk-fleet через adb).
- [`tools/MDM-DEVICE-CONTROL-CONTRACT.md`](https://github.com/daphate/tactical-ar-hud/blob/master/tools/MDM-DEVICE-CONTROL-CONTRACT.md) — wire contract между MDM и client'ом.
