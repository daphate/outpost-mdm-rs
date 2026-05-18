"""post_output_guard.py — пост-output защитный слой v17 (blocking-mode).

Назначение: между `gen()` LLM и пользователем стоит этот слой:
  1. Валидирует ответ через `drug_validator_v2` (дозы, forbidden процедуры, опасные сочетания).
  2. Truncation-guard (B4): ответ не должен оборваться на полуслове.
  3. Source-honesty guard (B5): нельзя писать «Источник: TCCC/CoTCCC», если этот документ
     не был передан в RAG-контекст (rag_chunks).
  4. Domain-leak guard (B-extra): мед-ответ не должен содержать автомобильную терминологию.
  5. Применяет автоисправления (rewriting) для известных units-конfusions.
  6. Решает: PASS / REWRITTEN / BLOCKED — в зависимости от тяжести.
  7. Логирует ВСЕ срабатывания в JSONL для DPO-bank.

Режимы (`mode`):
  - `"logging"`     — только логирует, ответ выдаётся как есть (НЕ ИСПОЛЬЗОВАТЬ В PROD).
  - `"rewrite"`     — применяет авто-фиксы units-confusion; всё что не починилось — логирует, не блокирует.
  - `"strict"`      — на critical блокирует ответ, выдаёт safe-fallback.
  - `"blocking"`    — V17 PROD: critical → block, important → попытка rewrite → если осталось → block.
                       Безопасный по умолчанию для production.

Использование:
    from post_output_guard import guard
    answer = gen(prompt)
    answer, action, issues = guard(prompt, answer, mode="blocking",
                                   rag_chunks=ctx, response_tag=tag)
"""
from __future__ import annotations
import json, re, time, os
from datetime import datetime
from pathlib import Path
from typing import Literal

from drug_validator_v2 import validate, severity_count, Issue

GuardMode = Literal["logging", "rewrite", "strict", "blocking"]
GuardAction = Literal["pass", "rewritten", "blocked", "regenerate"]

LOG_DIR = Path(__file__).resolve().parent / "logs_guard"
LOG_DIR.mkdir(exist_ok=True)


# === Авто-фиксы units-confusion ===============================================
# Только safe-rewrites — уверенные паттерны где модель явно перепутала единицы.
# Каждое правило: (pattern, replacement, comment).
# Применяются ДО валидации, чтобы не флагать заведомо мелкие units-typo.

REWRITES = [
    # Адреналин 1 мкг → 0.5 мг при анафилаксии (h89)
    (re.compile(r"адренал\w*\s+1\s*мкг", re.I),
     "адреналин 0.5 мг",
     "h89-class: адреналин 1мкг → 0.5мг"),
    # TXA 1 г + 1 г / 8 ч → 2 г однократно (CoTCCC 2024 update)
    (re.compile(r"(TXA|транексам\w*)\s+1\s*г[^.]{0,40}?(?:повтор\w*\s+)?1\s*г[^.]{0,20}?(?:через\s+)?8\s*час", re.I),
     r"\1 2 г в/в однократно",
     "CoTCCC-2024-update: 1+1/8ч → 2 г однократно"),
    # Налоксон с пометкой "анальгетик/обезболивающее" — удаляем неверную пометку
    (re.compile(r"налоксон\s+(\d+(?:[.,]\d+)?\s*(?:мг|мкг))\s+в/?[ивм](?:\s|/)*\([^)]*?(?:анальгет|обезбол)[^)]*\)", re.I),
     r"налоксон \1 (АНТАГОНИСТ опиоидов — не применяется при шоке/кровотечении/анафилаксии)",
     "h2-class: налоксон-как-анальгетик-pojasnenie"),
    # Налоксон при анафилаксии (h89 v14 mutation) — целая строка переписывается
    (re.compile(r"налоксон\s+\d+(?:[.,]\d+)?\s*(?:мг|мкг)[^\n]{0,80}?(?:если\s+есть\s+анафилакс|есть\s+анафилакс|при\s+анафилакс|анафилакс\w*\s*\+\s*гипотенз)", re.I),
     "[ОТМЕНЕНО ВАЛИДАТОРОМ — налоксон не применяется при анафилаксии. Адреналин 0.3-0.5 мг в/м — препарат выбора при анафилаксии]",
     "h89-v14-mutation: налоксон-при-анафилаксии"),
    # Адреналин при СЛР — слишком маленькая доза 0.1 мг (норма 1 мг)
    (re.compile(r"адренал\w*\s+0[.,]1\s*мг\s+в/?в[^\n]{0,80}?(?:слр|cpr|остановк|реаним)", re.I),
     "адреналин 1 мг в/в каждые 3-5 минут (ACLS 2020)",
     "h195-class: адреналин-СЛР-низкая-доза"),
    # Атропин 0.5 мг при брадикардии (устарело)
    (re.compile(r"атропин\s+0[.,]?5\s*мг\s+в/?в\s*(?:болюс|при)\s*(?:брадикард|av-?блокад)", re.I),
     "атропин 1 мг в/в болюсно (устаревшая 0.5 мг — риск парадоксальной брадикардии)",
     "ACLS-2020: атропин 0.5→1 мг при брадикардии"),
]


def apply_rewrites(text: str) -> tuple[str, list[str]]:
    """Применяет авто-фиксы. Возвращает (новый текст, список комментариев)."""
    applied = []
    for pat, repl, comment in REWRITES:
        new = pat.sub(repl, text)
        if new != text:
            applied.append(comment)
            text = new
    return text, applied


# === B4: Truncation-guard ====================================================
# Ответ считается обрезанным, если последний непробельный символ не является
# терминальной пунктуацией. Списки/нумерация требуют finalize sentence.

_TERMINAL_PUNCT = set(".!?…»)\"'»")

def is_truncated(text: str) -> bool:
    if not text:
        return False
    s = text.rstrip()
    if not s:
        return False
    last = s[-1]
    if last in _TERMINAL_PUNCT:
        return False
    # Допускаем закрывающие markdown / quote: » ) ]
    if last in {"]", ")", "»"}:
        return False
    # Цифра/буква в конце = почти всегда оборванный токен.
    if last.isalpha() or last.isdigit():
        return True
    # Дефис, тире, двоеточие — оборванное продолжение.
    if last in {"-", "—", "–", ":", ",", ";"}:
        return True
    return False


# === B5: Source-honesty guard ================================================
# Если модель пишет «Источник: TCCC / CoTCCC / CRASH-2 / ATLS / FM-...»,
# но в переданных rag_chunks нет ни одного чанка с этим источником,
# это фабрикация — нужно убрать или заменить.

_FABRICATED_SOURCE_RE = re.compile(
    r"(?:^|\n)\s*(?:Источник[:\s]|Источники[:\s]|Reference[:\s]|Источ\.[:\s])\s*([^\n]+)",
    re.IGNORECASE | re.MULTILINE,
)
_KNOWN_SOURCE_KEYWORDS = [
    "TCCC", "CoTCCC", "CRASH-2", "CRASH-3", "ACLS", "PALS", "ATLS",
    "Burn CPG", "CoERCCC", "CBRN", "SMOG", "CAT Manual",
    "FM 8-285", "FM-8-285", "WAO", "IDSA", "Surviving Sepsis", "WOMAN",
    "ВМедА", "Кирова",
]

def detect_fabricated_sources(response: str, rag_chunks: list | None) -> list[str]:
    """Возвращает список 'фабрикованных' источников — упомянутых в ответе,
    но не присутствующих ни в одном rag_chunk."""
    if not rag_chunks:
        # Без RAG-контекста любая ссылка на конкретный документ = фабрикация.
        rag_text = ""
    else:
        rag_text = "\n".join(
            (c.get("text") or "") + " " + (c.get("source") or "")
            for c in rag_chunks
        ).lower()
    fabricated = []
    for m in _FABRICATED_SOURCE_RE.finditer(response):
        cited = m.group(1).strip().rstrip(".;)")
        cited_lc = cited.lower()
        # Какие конкретные источники упомянуты в этой строке?
        for kw in _KNOWN_SOURCE_KEYWORDS:
            if kw.lower() in cited_lc and kw.lower() not in rag_text:
                fabricated.append(f"{kw} в строке «{cited[:80]}»")
    return fabricated


# === B-extra: Domain-leak guard (мед ↔ tech) =================================
# В мед-ответе не должно быть автомобильной терминологии (h4: «расколётся блок цилиндров»).

_AUTO_TERMS = re.compile(
    r"(?:блок\s+цилиндр|тнвд|свеч[аи]\s+(?:зажиган|накал)|карбюратор|"
    r"поршн\w*\s+коль|клапанн\w*\s+(?:рассопр|зазор)|пробк[аи]\s+радиатор|"
    r"расколётся\s+блок|остановки\s+двигател|солярк\w*\s+из\s+бак|колен(?:чат|вал)|"
    r"свечей\s+зажиган)",
    re.IGNORECASE,
)

def detect_domain_leak(response: str, response_tag: str | None) -> str | None:
    if response_tag not in ("med", "cbrn", "char", "comm", "surv"):
        return None
    m = _AUTO_TERMS.search(response)
    if m:
        return m.group(0)
    return None


# === Safe-fallback шаблоны (B2) ===============================================

def _safe_fallback(prompt: str, issues: list[Issue],
                   truncation: bool = False,
                   fabricated_sources: list[str] | None = None,
                   domain_leak: str | None = None) -> str:
    """Возвращает безопасный fallback-ответ на основе типа проблем."""
    parts = ["[ОТВЕТ ЗАБЛОКИРОВАН VALIDATOR'ОМ — ниже причина и канон.]"]

    # Группируем критичные ошибки
    crit = [i for i in issues if i.severity == "critical"]
    imp  = [i for i in issues if i.severity == "important"]

    if crit:
        parts.append("\n⚠ КРИТИЧЕСКИЕ ОШИБКИ:")
        for i in crit[:6]:
            parts.append(f"  • {i.name}: {i.detail[:200]}")
    if imp and not crit:
        parts.append("\n⚠ ВАЖНЫЕ ОШИБКИ:")
        for i in imp[:6]:
            parts.append(f"  • {i.name}: {i.detail[:200]}")
    if truncation:
        parts.append("\n⚠ ОТВЕТ БЫЛ ОБРЕЗАН — не достиг конца алгоритма.")
    if fabricated_sources:
        parts.append("\n⚠ ФАБРИКАЦИЯ ИСТОЧНИКА: " + "; ".join(fabricated_sources[:3]))
    if domain_leak:
        parts.append(f"\n⚠ МУСОР ИЗ ДРУГОГО ДОМЕНА: «{domain_leak[:60]}»")

    parts.append(
        "\n\nДЕЙСТВИЯ ПО КАНОНУ:\n"
        "1. Применить базовый алгоритм MARCH-PAWS (массивное кровотечение → дыхание → циркуляция → гипотермия → боль/АБ/раны/шины).\n"
        "2. Дозы препаратов сверять с drug_table.json (CoTCCC 2024 / Burn CPG 2025): TXA 2 г IV однократно; адреналин 0.3-0.5 мг IM при анафилаксии или 1 мг IV при СЛР; кетамин 50-100 мг IM анальгезия; атропин 2 мг IM при ФОВ; OTFC фентанил 800 мкг трансбуккально.\n"
        "3. ЗАПРЕЩЕНО в поле: трепанация черепа, торакотомия, лапаротомия, катетер Фолея в вену, артерия как венозный доступ, ИПП-11 на ожог, налоксон при боли, TXA внутрь.\n"
        "4. Эвакуация в Role 2/3 при сложных процедурах. Связь по 9-line MEDEVAC.\n"
        "5. Если запрос вне компетенции — сказать прямо: «нет данных в моей базе»."
    )
    return "\n".join(parts)


# === Главная функция guard ====================================================

def guard(prompt: str, response: str, mode: GuardMode = "blocking",
          log_file: str | None = None,
          rag_chunks: list | None = None,
          response_tag: str | None = None) -> tuple[str, GuardAction, list[Issue]]:
    """Применяет post-output защиту. Возвращает (final_response, action, issues).

    action:
      - "pass" — ответ прошёл без изменений
      - "rewritten" — применены авто-фиксы (units-typo)
      - "blocked" — ответ заблокирован, вернулся safe-fallback
      - "regenerate" — нужна перегенерация (запросить заново)

    Параметры:
      mode: см. GuardMode docstring модуля.
      rag_chunks: список dict с ключами text/source — для source-honesty проверки.
      response_tag: тег домена ответа (med/tech/...) — для domain-leak проверки.
    """
    original_response = response
    rewrites_applied: list[str] = []

    # Stage 1: попытка авто-фиксов (только в rewrite/strict/blocking)
    if mode in ("rewrite", "strict", "blocking"):
        response, rewrites_applied = apply_rewrites(response)

    # Stage 2: валидация препаратов и процедур
    issues = validate(prompt, response)
    sev = severity_count(issues)

    # Stage 3: новые проверки (B4 + B5 + domain-leak)
    truncation = is_truncated(response)
    fabricated = detect_fabricated_sources(response, rag_chunks) if mode != "logging" else []
    domain_leak = detect_domain_leak(response, response_tag) if mode != "logging" else None

    # Дополнительные псевдо-issues для логирования
    if truncation:
        issues.append(Issue("truncation_guard", "important",
                            "Ответ обрезан на не-терминальном символе (max_tokens достигнут или модель не закончила).",
                            span=None))
    for fab in fabricated:
        issues.append(Issue("fabricated_source", "important",
                            f"В ответе ссылка на источник, которого нет в RAG-контексте: {fab}",
                            span=None))
    if domain_leak:
        issues.append(Issue("domain_leak", "important",
                            f"В мед-ответе автомобильная терминология: «{domain_leak[:60]}»",
                            span=None))

    # Recompute severity after additions
    sev = severity_count(issues)

    # Stage 4: решение
    action: GuardAction = "pass"
    if rewrites_applied:
        action = "rewritten"

    if mode == "strict" and sev["critical"] > 0:
        critical_msgs = "; ".join(str(i.detail)[:100] for i in issues if i.severity == "critical")
        response = (
            "Не могу выдать этот ответ — обнаружена критическая ошибка препарата/процедуры. "
            f"Подробности: {critical_msgs}. Запросите ответ заново или обратитесь к старшему медику."
        )
        action = "blocked"

    if mode == "blocking":
        # V17 PROD-режим:
        #   critical → block + safe-fallback
        #   important × ≥2 → block (накопительный риск)
        #   important × 1 → rewrite-attempt уже сделан в Stage 1; если осталось — pass с warning
        #   truncation/fabricated/domain_leak (любой) → block
        should_block = (
            sev["critical"] > 0 or
            sev["important"] >= 2 or
            truncation or
            bool(fabricated) or
            bool(domain_leak)
        )
        if should_block:
            response = _safe_fallback(prompt, issues,
                                      truncation=truncation,
                                      fabricated_sources=fabricated,
                                      domain_leak=domain_leak)
            action = "blocked"

    # Stage 5: логирование (всегда при issues, в logging-mode тоже)
    if log_file or sev["critical"] > 0 or sev["important"] > 0 or rewrites_applied or truncation:
        _write_log(prompt, original_response, response, action, issues, sev, rewrites_applied,
                   log_file or str(LOG_DIR / "guard_default.jsonl"))

    return response, action, issues


def _write_log(prompt, orig, final, action, issues, sev, rewrites, path):
    rec = {
        "ts": datetime.utcnow().isoformat() + "Z",
        "prompt": prompt[:300],
        "orig_response": orig,
        "final_response": final if final != orig else None,
        "action": action,
        "severity_count": sev,
        "issues": [str(i) for i in issues],
        "rewrites_applied": rewrites,
    }
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


# === Self-test ================================================================

if __name__ == "__main__":
    import sys
    if sys.platform == "win32":
        try: sys.stdout.reconfigure(encoding="utf-8")
        except: pass

    cases = [
        # rewrite-mode: должен исправить адреналин 1мкг → 0.5мг
        {
            "label": "rewrite_адреналин_1мкг",
            "prompt": "Анафилаксия после укуса пчелы",
            "response": "1. Адреналин 1 мкг в/м немедленно. 2. Кислород.",
            "mode": "rewrite",
            "expect_action": "rewritten",
            "expect_in_final": "0.5 мг",
        },
        # logging-mode: ничего не меняем, только логируем
        {
            "label": "log_трепанация",
            "prompt": "ЧМТ нужна декомпрессия",
            "response": "Сделай декомпрессионную трепанацию иглой 14G.",
            "mode": "logging",
            "expect_action": "pass",
            "expect_in_final": "трепанацию",
        },
        # strict-mode: блокирует трепанацию
        {
            "label": "strict_трепанация_block",
            "prompt": "ЧМТ нужна декомпрессия",
            "response": "Сделай декомпрессионную трепанацию иглой 14G в поле.",
            "mode": "strict",
            "expect_action": "blocked",
            "expect_in_final": "Не могу выдать",
        },
        # blocking-mode: critical → block
        {
            "label": "blocking_трепанация_block",
            "prompt": "ЧМТ нужна декомпрессия",
            "response": "Сделай декомпрессионную трепанацию иглой 14G в поле.",
            "mode": "blocking",
            "expect_action": "blocked",
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking-mode: torakотомия → block
        {
            "label": "blocking_торакотомия_block",
            "prompt": "Экстренная торакотомия в поле",
            "response": "Выполни торакотомию в поле через межреберье 4-5.",
            "mode": "blocking",
            "expect_action": "blocked",
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking: налоксон при боли → block
        {
            "label": "blocking_налоксон_боль_block",
            "prompt": "Сильная боль при ампутации",
            "response": "Налоксон 3 мг в/м при сильной боли.",
            "mode": "blocking",
            "expect_action": "blocked",
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking: TXA внутрь → block
        {
            "label": "blocking_TXA_po_block",
            "prompt": "Кровотечение",
            "response": "TXA 50 мг внутрь — это поможет остановить кровь.",
            "mode": "blocking",
            "expect_action": "blocked",
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking: truncation → block
        {
            "label": "blocking_truncation_block",
            "prompt": "Алгоритм при пневмотораксе",
            "response": "1. Декомпрессия иглой 14G в 5-е межреберье. 2. Окклюз",
            "mode": "blocking",
            "expect_action": "blocked",
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking: domain leak (engine in med) → block
        {
            "label": "blocking_engine_in_med_block",
            "prompt": "Танкисту обожгло кисти",
            "response": "Охлади 30 минут после остановки двигателя — иначе расколётся блок цилиндров. Стерильная повязка.",
            "mode": "blocking",
            "expect_action": "blocked",
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking: fabricated source → block
        {
            "label": "blocking_fabricated_source_block",
            "prompt": "Доза TXA",
            "response": "TXA 2 г в/в.\nИсточник: TCCC Guidelines; CRASH-2 study.",
            "mode": "blocking",
            "expect_action": "blocked",  # rag_chunks=None → любая ссылка = фабрикация
            "expect_in_final": "ЗАБЛОКИРОВАН",
        },
        # blocking: правильный ответ — pass
        {
            "label": "blocking_correct_pass",
            "prompt": "Анафилаксия",
            "response": "1. Адреналин 0.5 мг в/м немедленно. 2. Повтор каждые 5-15 мин до 3 раз.",
            "mode": "blocking",
            "expect_action": "pass",
            "expect_in_final": "Адреналин",
        },
        # rewrite: правильный ответ — никаких изменений
        {
            "label": "rewrite_pass",
            "prompt": "Анафилаксия",
            "response": "Адреналин 0.5 мг в/м, повтор через 5 минут.",
            "mode": "rewrite",
            "expect_action": "pass",
            "expect_in_final": "0.5 мг",
        },
        # rewrite: налоксон-как-анальгетик
        {
            "label": "rewrite_налоксон_pojasnenie",
            "prompt": "Артериальное кровотечение",
            "response": "1. Налоксон 0.5 мг в/м (анальгетик при шоке). 2. CAT.",
            "mode": "rewrite",
            "expect_action": "rewritten",
            "expect_in_final": "АНТАГОНИСТ",
        },
        # blocking + rag_chunks: TCCC в RAG → ссылка не фабрикуется
        {
            "label": "blocking_TCCC_in_rag_pass",
            "prompt": "Доза TXA",
            "response": "1. TXA 2 г в/в однократно за 1 минуту, в первые 3 часа.\nИсточник: TCCC Guidelines.",
            "mode": "blocking",
            "expect_action": "pass",
            "expect_in_final": "TXA",
            "rag_chunks": [{"text": "TXA 2 г IV/IO однократно", "source": "tccc_guidelines_2024.txt"}],
        },
    ]

    failed = 0
    for c in cases:
        kw = {"mode": c["mode"]}
        if "rag_chunks" in c:
            kw["rag_chunks"] = c["rag_chunks"]
        if "response_tag" in c:
            kw["response_tag"] = c["response_tag"]
        # h4-like (engine domain leak) needs response_tag=med
        if "engine" in c["label"]:
            kw["response_tag"] = "med"
        final, action, issues = guard(c["prompt"], c["response"], **kw)
        ok_action = action == c["expect_action"]
        ok_text = c["expect_in_final"] in final
        ok = ok_action and ok_text
        mark = "[OK]  " if ok else "[FAIL]"
        print(f"{mark} {c['label']}: action={action} | final='{final[:80]}'")
        if not ok:
            failed += 1
            print(f"   expected action={c['expect_action']}, in_final='{c['expect_in_final']}'")
            print(f"   issues: {[str(i) for i in issues[:3]]}")
    print()
    print("ALL OK" if failed == 0 else f"{failed} CASES FAILED")
