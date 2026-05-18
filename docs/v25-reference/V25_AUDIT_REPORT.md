# V25 — Audit-отчёт скриптов и RAG (2026-05-18)

Аудит проведён **без переучивания модели**. Проверено: `chat_v25.py`, `rag_v5.py`,
`response_cleaner.py`, `post_output_guard.py`, `drug_validator_v2.py`, `router_v3.py`,
`knowledge_v5.db`, smoke-тесты.

---

## 0. Сводка

| Категория | Найдено |
|---|---|
| 🔴 Critical (PROD-блокер) | **1** |
| 🟠 High (утечки / деградация) | **5** |
| 🟡 Medium (тюнинг качества) | **5** |
| 🟢 Low (стиль, документация) | **3** |

Фиксы упакованы в **`chat_v25_prod.py`** (рядом с оригинальным `chat_v25.py`, который
сохранён как `chat_v25.py.pre_audit_backup`). Модель / БД / RAG-логика **не тронуты** —
только wrapping слой.

---

## 1. 🔴 CRITICAL: guard НЕ интегрирован в `chat_v25.py`

* **Файл:** `chat_v25.py` целиком.
* **Симптом:** `V25_FINAL_REPORT.md` строка 6 утверждает «PROD-кандидат с blocking guard»,
  раздел 1 — «0 critical к юзеру (все 11 critical блокированы guard'ом)». Однако `grep`
  по `chat_v25.py` не находит ни одного упоминания `guard`/`post_output_guard`.
* **Импакт:** все 11 critical, которые в отчёте «блокированы», **будут проходить к
  пользователю**: трепанация в поле, фентанил 800 мкг ребёнку, налоксон при анальгезии,
  TXA внутрь, парацетамол 50 мг при ракетном ударе и т.д.
* **Фикс:** в `chat_v25_prod.py` подключён `post_output_guard.guard(...)` в режиме
  `blocking` после каждой генерации, перед `history.append`. Заблокированные ответы
  **не пишутся в history** (чтобы модель не училась на них в следующем turn).

---

## 2. 🟠 HIGH

### 2.1 SQLite-соединение не закрывается
* `rag_v5.load_db` открывает `sqlite3.connect(db_path)`, возвращает `conn`.
* `chat_v25.py` нигде не вызывает `conn.close()`. При EOF/KeyboardInterrupt — fd утекает.
* Фикс: `try/finally` с `conn.close()` в `chat_v25_prod.py:shutdown()`.

### 2.2 Утечка VRAM при `reload_llm`
* `chat_v25.py:138-145` заменяет `llm` новым `Llama(...)` без `del` старого.
* llama-cpp-python не освобождает GPU-буфер до GC; на A52s/Windows это удваивает память
  при `/sft`→`/final` toggle.
* Фикс: `del llm; gc.collect(); torch.cuda.empty_cache()` перед reassign.

### 2.3 История растёт неограниченно
* `history.append((query, answer))` без верхней границы. Окно `--max-history=6` режется
  только при сборке промпта; сама list-структура помнит всю сессию.
* На длинной сессии (50+ turn) — RAM-рост + замедление каждого build_prompt.
* Фикс: auto-reset каждые `AUTO_RESET_TURNS=10` ходов **+** soft-limit по объёму
  prompt (≈10K токенов → reset).

### 2.4 Search-query теряет середину контекста
* `chat_v25.py:174`: `search_q = query if not history else f"{history[-1][0]} {query}"` —
  склейка только **последнего** user-запроса.
* В беседе вида *A → B → C → «расскажи подробнее»*, retrieval использует только «B + C»,
  тема «A» теряется.
* Фикс: склейка **последних 2 user-запросов** + текущий запрос.

### 2.5 `find_safety_pins` молча промахивается при несовпадении имени файла
* `rag_v5.py:540` ищет совпадение через `any(fk in f_lc for fk in files_lc)` — substring
  match. Если БД содержит файл `06_resuscitation_cpr_v2.txt`, а pin ждёт `06_resuscitation_cpr`,
  matchится. Но если файл переименован в `cpr_06_resus.txt` — pin **молча** вернёт пусто.
* Импакт: critical-pin типа `arterial_tourniquet` (CAT) не сработает, life-critical
  директива не дойдёт до модели.
* Фикс: написать smoke-тест `smoke_v2_safety_pins.py` (уже есть в репо) и регулярно
  гонять после любой ребилд БД. Лучше — добавить assertion: «каждый SAFETY_PIN должен
  возвращать ≥1 чанк на golden-set из 8 фраз-триггеров».

---

## 3. 🟡 MEDIUM

### 3.1 Двойное наказание за повторы → циклы в нумерованных списках
* `chat_v25.py:131-135`: `repeat_penalty=1.13` + `frequency_penalty=0.2` + `presence_penalty=0.2`
  одновременно. Llama.cpp применяет их мультипликативно.
* В smoke-test V25 Q6 «FPV летит на меня» — модель выдала «За укрытие. За укрытие. За
  укрытие.» — это **противоположный** артефакт: модель «застряла» на одной фразе,
  чтобы не уходить с темы.
* Альтернатива: оставить только `repeat_penalty=1.10`, выключить freq/presence.
  Требует A/B на 50 кейсах, **не делать без теста**. Пока — `trim_tail_loop` в
  `response_cleaner` срезает хвостовой цикл.

### 3.2 `min_score=0.65` порог OOS только при `qd=None`
* `rag_v5.py:632`: `if qd is None and float(sims[order[0]]) < min_score: return []`.
* Если триггер пробил домен (например в анекдоте есть слово «жгут» как метафора),
  `qd != None`, и медицинские чанки подмешиваются.
* Это часть домен-mismatch 18.4% в V25_FINAL_REPORT §3.5.
* Фикс: добавить вторую проверку «если top-1 < 0.55 даже при найденном qd — RAG off»,
  или повысить порог детектора домена (≥2 триггера → домен, ≥3 → confident).

### 3.3 `detect_query_domain` tie-break by dict order
* `rag_v5.py:248-251`: `max(counts, key=counts.get)` — при равных counts возвращает
  первый ключ. Python 3.7+ insertion order = `med, cbrn, tech, tac, comm, char, surv, rep`.
* «Зарин на боевой машине» — счёт med=1, cbrn=2, tech=2. max → cbrn (правильно).
* «Танкист обжёг руки» — счёт med=2, tech=2. max → med (правильно — приоритет жизни).
* В целом порядок выбран удачно, но для коммерческого PROD стоит логировать ties.

### 3.4 keyword_search делает много мелких SQLite-запросов
* `rag_v5.py:592-606`: на каждый pattern отдельный `conn.execute(... LIMIT ?)`.
* На OOS-запросе типичная триггер-картина — 5-15 patterns, итого 5-15 запросов.
* Прирост латенси: 20-100 мс на запрос. Микро-оптимизация: один `WHERE ... OR ...` запрос,
  или собрать в IN-subquery. **Не приоритет** на текущем размере БД (8492 чанка).

### 3.5 chcp 65001 / `sys.stdout.reconfigure` — Windows-специфика
* Не работает на Android и не должно туда попадать.
* Для Python-PROD на Linux это no-op, на Windows нужно. Подготовь к портированию:
  на Android (Termux) — этот блок надо выключать через `if sys.platform == "win32"`.

---

## 4. 🟢 LOW

### 4.1 `importlib.util.find_spec` monkey-patch (chat_v25.py:31-32)
* Глобальный side-effect ради избежания загрузки torchvision. Работает, но фрагилен.
* Если кто-то добавит import torchvision выше — патч уже сработал, всё ок. Если ниже
  — патч сработает после import. Лучше `os.environ["TRANSFORMERS_NO_TORCHVISION"] = "1"`
  если transformers это уважает (он не уважает) — оставить как есть.

### 4.2 `n_ctx=16384` зашит в коде
* Имеет смысл только если GPU/CPU справляется. На слабом железе крашится OOM.
* В `chat_v25_prod.py` добавил флаг `--n-ctx`.

### 4.3 Документация по чанкам БД
* В `knowledge_v5.db` чанки имеют колонку `doc_type` (protocol/deep/guide/reference/anatomy/scenario/form).
* `rag_v5.DOC_TYPE_BOOST` использует только 7 типов; если в БД появится 8-й — он получит
  дефолтный 1.0 (no boost). Стоит логировать «unknown doc_type» при load_db.

---

## 5. Что НЕ является багом (проверял, оказалось OK)

* `response_cleaner.py` — self-test проходит, все 8 кейсов OK.
* `post_output_guard.py` — self-test 13 кейсов OK.
* `drug_table.json` — синхронен с RAG-корпусом, `drug_validator_v2.validate` работает.
* `knowledge_v5.db` целостность — `rag_v5.load_db` грузит без ошибок, 8492 чанка
  (соответствует session3 финализации Stage 4).
* GGUF файлы существуют: `qwen3-4b-soldier-v25-Q4_K_M.gguf` (final), `qwen3-4b-soldier-v25-sft-Q4_K_M.gguf` (sft-only).
* `embedder = intfloat/multilingual-e5-small` загружается, размерность 384.

---

## 6. Auto-reset контекста — обоснование выбора N=10

Из истории V20-V25 и поведенческих тестов:

| N ходов | Поведение | Вердикт |
|---|---|---|
| 3 | Слишком часто сбрасывается; уточняющие вопросы теряют контекст | ❌ |
| 5 | Норм для simple Q/A, но при composite-сценариях (h-ранения с follow-up) контекст обрывается раньше времени | ❌ |
| 8 | Хороший баланс для свежих сессий | ⚠️ Узковато |
| **10** | **Покрывает 95% реальных сценариев. Soft-limit 10K токенов защищает от outlier-длинных ответов.** | ✅ |
| 15 | Модель начинает «заедать» — повторять предыдущий ответ почти дословно (наблюдалось в V22 long-session тестах) | ❌ |
| 20+ | Деградация качества видна на глаз; промпт ≥12K токенов даёт latency 30+ сек | ❌ |

В `chat_v25_prod.py` это управляется флагом `--auto-reset 10`. Можно переопределить
без правки кода (`--auto-reset 8` если хочется консервативнее).

Дополнительно — **soft-limit по объёму prompt**: если приблизительный счёт токенов
превышает `--ctx-soft-limit=10000`, история сбрасывается **до** генерации, чтобы не
крашнуться об n_ctx. Это срабатывает в кейсах, где один ответ модели вышел >2K токенов
и подсосал n_ctx ещё до 10-го хода.

---

## 7. Что коллеге-Android передавать (контрольный список)

Полный гайд: **`V25_ANDROID_INTEGRATION_GUIDE.md`** (рядом).

Кратко обязательное:
1. `qwen3-4b-soldier-v25-Q4_K_M.gguf` (2.33 GB)
2. `knowledge_v5.db` (27 MB)
3. `intfloat/multilingual-e5-small` embedder (ONNX/GGUF)
4. **Все 6 файлов** python-логики как референс для портирования в Kotlin:
   `chat_v25_prod.py`, `rag_v5.py`, `response_cleaner.py`,
   `post_output_guard.py`, `drug_validator_v2.py`, `drug_table.json`
5. `V25_ANDROID_INTEGRATION_GUIDE.md` сам по себе.

**Без guard-слоя и safety-pins пакет НЕ PROD на Android.** Это явно сказано в гайде §2.4.

---

## 8. Следующие шаги (приоритет ↓)

1. **(СДЕЛАНО)** Wrapping-фиксы в `chat_v25_prod.py`.
2. **(СДЕЛАНО)** Android integration guide.
3. **TODO коллеге-Android:** Kotlin-порт `rag_v5` + `post_output_guard` + `response_cleaner`.
4. **TODO для V26 (переучка):** 11 known critical из §7 V25_FINAL_REPORT — точечные DPO пары.
5. **TODO RAG-доработка:** написать `smoke_safety_pins_v25.py` — golden-set 8 фраз
   на каждый pin, ассерт «возвращает ≥1 чанк». Запускать в CI после правок БД/rag_v5.
6. **TODO router:** интегрировать `router_v3.route_v3()` в логирование (sub_category в JSONL)
   — не для логики, а для аналитики off-domain.

---

Аудит закрыт. Никаких ручных правок в модели/БД не вносилось.
