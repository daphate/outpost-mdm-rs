-- Заполнить settings_json default-конфигурации реальными значениями.
--
-- Migration 0019 создавала seed-config «По умолчанию» с пустым `{}`. Это
-- было place-holder'ом. Реальные значения берутся из:
--   - AR Hud client schema: `tools/MDM-DEVICE-CONTROL-CONTRACT.md §1.3`
--     (mapping `JSON key → ModelPreferences.setter`)
--   - Verified enum variants в Kotlin: ModelPreferences.kt, AnswerMode.kt,
--     PipelineLog.kt в `daphate/tactical-ar-hud` rc42+
--
-- Default policy — conservative outpost-ready конфиг:
--
-- preferred_llm = Soldier v25 (latest, raskat'нут 2026-05-18)
-- preferred_translator_llm = Qwen2.5-3B (light translator, T1+)
-- preferred_stt = whisper-base (works on T0+, balance quality/perf)
-- preferred_vlm = Qwen2-VL-2B (multilingual, T0+)
-- tts_mode = WakeWordOnly (озвучка не на каждое сообщение чтобы не
--   создавать звуковой ад в окопе; включается на wake-word триггере)
-- wake_word_enabled = true (главная UX-фича)
-- answer_mode = Auto (client сам решает Search/FastAssistant/FullAssistant
--   по сложности запроса)
-- translator_mode = Local (offline-first для полевых сценариев)
-- translator_cloud_enabled = false (privacy + offline guarantees)
-- translator_audio_mode = SpeakerphoneBoth (двое лицом к лицу, телефон
--   между ними — out-of-the-box сценарий)
-- show_build_badge = false (production, без отладочной плашки)
-- cpu_thread_count = 0 (auto — client выбирает по DeviceCapabilities.tier)
-- log_level = VERBOSE (beta-mode согласно CLIENT-TELEMETRY-CONTRACT.md §1:
--   шлём полные тексты promtp'ов и ответов LLM/translator/VLM для отладки
--   качества модели)
-- telemetry_enabled = true (admin хочет видеть в Grafana)
--
-- telemetry_endpoint специально не указан — выдаётся в enroll-response,
-- per CONTRACT §1.3 «Server-side НИКОГДА не отправляет telemetry_token /
-- telemetry_endpoint через update-config».
--
-- WHERE-фильтр на пустой '{}' — идемпотентно. Если admin уже что-то задал
-- в settings_json (либо через UI, либо через прямой SQL), не перезаписываем.

UPDATE configurations
SET settings_json = '{
  "preferred_llm": "qwen3-4b-soldier-v25-Q4_K_M.gguf",
  "preferred_translator_llm": "qwen2.5-3b-instruct-q4_k_m.gguf",
  "preferred_stt": "ggml-base-q5_1.bin",
  "preferred_vlm": "qwen2-vl-2b-instruct-q4_k_m.gguf",
  "tts_mode": "WakeWordOnly",
  "wake_word_enabled": true,
  "answer_mode": "Auto",
  "translator_mode": "Local",
  "translator_cloud_enabled": false,
  "translator_audio_mode": "SpeakerphoneBoth",
  "show_build_badge": false,
  "cpu_thread_count": 0,
  "log_level": "VERBOSE",
  "telemetry_enabled": true
}',
    updated_at = datetime('now')
WHERE name = 'По умолчанию'
  AND (settings_json IS NULL OR settings_json = '{}' OR settings_json = '');
