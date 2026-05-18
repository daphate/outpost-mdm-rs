"""drug_validator_v2.py — пост-output валидатор препаратов/доз/процедур (Слой 1 V15).

Принципиальное отличие от v1 (`drug_dose_validator.py`):
  v1 — hard-coded списки в Python.
  v2 — внешняя таблица `drug_table.json`, синхронизированная с RAG-корпусом
       (`00_quick_dose_reference_card.txt`). Изменение канона = правка JSON,
       не правка кода.

Возможности:
  1. Detect drug-mention с правильным dose-binding (как в v1).
  2. Поддержка единиц мкг/мг/г/мл/мг/кг/мкг/кг/мг/мл (НЕ путает с дозой).
  3. Per-indication dose ranges (анафилаксия vs СЛР для адреналина).
  4. Forbidden indications (налоксон-как-анальгетик).
  5. Forbidden field procedures (трепанация в поле).
  6. Wrong combinations (коникотомия = фасциотомия, сукцинилхолин+гиперкалиемия).
  7. Возвращает Issue + suggested_replacement (для rewriting-режима).

Используется в pipeline:
  - `chat_v13.py` / `run_*.py`: после `gen()` → `validate(prompt, response)` →
    если есть critical → blocking + перегенерация / refusal.
  - В eval-сценариях: для оценки drug-error-rate как метрики.
"""
from __future__ import annotations
import json, re
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

CRITICAL = "critical"
IMPORTANT = "important"
MINOR = "minor"

DEFAULT_TABLE_PATH = Path(__file__).resolve().parent / "drug_table.json"


@dataclass
class Issue:
    name: str
    severity: str
    detail: str
    span: tuple[int, int] | None = None
    suggested_replacement: str | None = None

    def __str__(self):
        s = f"DRUG[{self.severity}]: {self.name} — {self.detail}"
        if self.suggested_replacement:
            s += f" | suggest: «{self.suggested_replacement[:80]}»"
        return s


# =============================================================================
# Лексеры
# =============================================================================

# Поддерживаем ВСЕ единицы включая per-kg / concentration. Сначала более длинные.
_UNIT_ALT = (
    "мкг/кг|мг/кг|мг/мл|мкг/мл|г/мл|г/л|"
    "мкг/мин|мг/мин|мкг/кг/мин|мг/кг/мин|"
    "ед/мин|единиц/мин|"
    "мкг|мг|\\bг\\b|\\bg\\b|\\bml\\b|\\bмл\\b|\\bмlitr|"
    "единиц|\\bед\\b|\\bЕД\\b|таблет|капл"
)
DOSE_RE = re.compile(
    r"(?P<num>\d+(?:[.,]\d+)?(?:\s*[-–]\s*\d+(?:[.,]\d+)?)?)\s*"
    r"(?P<unit>" + _UNIT_ALT + ")",
    re.IGNORECASE,
)

ROUTE_RE = re.compile(
    r"\b(?:в/?в|в/?м|в/?о|внутривенн\w*|внутримышеч\w*|внутрикостн\w*|"
    r"подкож\w*|перорал\w*|внутрь|сублингв\w*|интраназ\w*|ингаляц\w*|"
    r"наружн\w*|трансбукк\w*|буккал\w*|ректал\w*|"
    r"i/?v|i/?m|i/?o|s/?c|p/?o|\bin\b|odt|otfc)",
    re.IGNORECASE,
)

ROUTE_NORM = {
    "в/в": "iv", "вв": "iv", "внутривенн": "iv", "iv": "iv", "i/v": "iv",
    "в/м": "im", "вм": "im", "внутримышеч": "im", "im": "im", "i/m": "im",
    "в/о": "io", "во": "io", "внутрикостн": "io", "io": "io", "i/o": "io",
    "подкож": "sc", "sc": "sc", "s/c": "sc",
    "перорал": "po", "внутрь": "po", "po": "po", "p/o": "po",
    "сублингв": "sublingv", "трансбукк": "трансбукк", "буккал": "трансбукк",
    "интраназ": "in", "in": "in", "innazal": "in",
    "ингаляц": "ingaliac",
    "наружн": "naruzhno",
    "ректал": "rectum",
    "otfc": "otfc", "odt": "po",
}


def normalize_route(s: str) -> str:
    s = s.lower().replace(" ", "").replace("ё", "е")
    # обрезаем окончания (внутривенно → внутривенн)
    for stem, code in ROUTE_NORM.items():
        if s.startswith(stem):
            return code
    return ROUTE_NORM.get(s, s)


# Препараты, не входящие в whitelist но РАЗРЫВАЮЩИЕ dose-binding
# (соседние «парацетамол 50 мг» не должны цепляться к TXA-mention).
BINDING_BREAKERS = [
    r"метронидазол", r"ванкомицин", r"эритромицин", r"клиндамицин", r"гентамицин",
    r"норадреналин", r"допамин", r"добутамин", r"вазопрессин",
    r"декстроза", r"глюкоз[ыа]", r"гэк\b", r"гидроксиэтилкрахмал", r"маннитол",
    r"эуфиллин", r"димедрол", r"супрастин", r"тавегил", r"дифенгидрамин",
    r"гепарин", r"варфарин", r"эноксапарин", r"клексан",
    r"диклофенак", r"анальгин", r"метамизол", r"кеторолак", r"кетопрофен", r"дексалгин",
    r"цельная\s+кров", r"плазм[аы]", r"эритроцит", r"фактор\s+vii",
    r"\bnac\b", r"ацетилцистеин", r"флумазенил", r"сальбутамол",
    r"сукцинилхолин", r"рокурониум", r"векурониум", r"этомидат", r"тиопентал", r"пропофол",
    r"мидазолам", r"диазепам", r"ондансетрон",
    # Также: магний, калий, натрий (как соли в инфузиях) — числа рядом не drug-doses
    r"магни[йя]", r"калия?\s+хлорид", r"натри[йя]\s+хлорид",
]


# =============================================================================
# Загрузка таблицы
# =============================================================================

class DrugTable:
    def __init__(self, path: Path = DEFAULT_TABLE_PATH):
        with open(path, encoding="utf-8") as f:
            self.data = json.load(f)
        self.drugs = self.data["drugs"]
        self.forbidden_procs = self.data["forbidden_field_procedures"]
        self.wrong_combos = self.data["wrong_combinations"]
        # Объединённый regex для каждой группы паттернов препаратов
        self._compiled_drug_patterns = []
        for canon, info in self.drugs.items():
            for pat in info["patterns"]:
                try:
                    self._compiled_drug_patterns.append((canon, info, re.compile(pat, re.IGNORECASE)))
                except re.error as e:
                    raise ValueError(f"Bad pattern for {canon}: {pat!r} — {e}")
        self._compiled_breakers = [re.compile(p, re.IGNORECASE) for p in BINDING_BREAKERS]


# =============================================================================
# Извлечение упоминаний препаратов с правильным dose-binding
# =============================================================================

def _find_drug_spans(text: str, table: DrugTable) -> list[tuple[int, int, str, dict, str]]:
    """Returns sorted (start, end, canon_name, info_dict, matched_text)."""
    spans = []
    for canon, info, pat in table._compiled_drug_patterns:
        for m in pat.finditer(text):
            spans.append((m.start(), m.end(), canon, info, m.group(0)))
    spans.sort(key=lambda x: x[0])
    # Dedup по позиции и canon (один препарат → одно вхождение в точке)
    seen = set()
    uniq = []
    for s, e, c, i, t in spans:
        key = (s, c)
        if key in seen:
            continue
        seen.add(key)
        uniq.append((s, e, c, i, t))
    return uniq


def _find_breaker_spans(text: str, table: DrugTable) -> list[tuple[int, int]]:
    spans = []
    for pat in table._compiled_breakers:
        for m in pat.finditer(text):
            spans.append((m.start(), m.end()))
    return spans


def _extract_dose_value(num_str: str, unit: str) -> tuple[float, str, bool]:
    """Returns (value_in_canonical_unit, kind, is_range).
    kind: 'mass_mcg' / 'mass_mg' / 'mass_g' / 'volume_ml' / 'per_kg' / 'concentration' / 'rate' / 'units' / 'other'
    is_range: True если '5-10' (берём верхнюю границу для проверки max, нижнюю для min — упрощённо берём среднее).
    """
    is_range = ("-" in num_str) or ("–" in num_str)
    if is_range:
        parts = re.split(r"[-–]", num_str)
        n = (float(parts[0].replace(",", ".").strip()) + float(parts[1].replace(",", ".").strip())) / 2
    else:
        n = float(num_str.replace(",", "."))
    u = unit.lower().replace(" ", "")

    # per-kg / per-min — служебные, не абсолютная разовая
    if u in ("мкг/кг", "мг/кг"):
        return (n, "per_kg", is_range)
    if u in ("мг/мл", "мкг/мл", "г/мл", "г/л"):
        return (n, "concentration", is_range)
    if u in ("мкг/мин", "мг/мин", "мкг/кг/мин", "мг/кг/мин", "ед/мин", "единиц/мин"):
        return (n, "rate", is_range)
    # масса
    if u == "мкг":
        return (n, "mass_mcg", is_range)
    if u in ("мг", "mg"):
        return (n, "mass_mg", is_range)
    if u in ("г", "g"):
        return (n, "mass_g", is_range)
    if u in ("мл", "ml", "мlitr"):
        return (n, "volume_ml", is_range)
    if u in ("ед", "единиц", "од"):
        return (n, "units", is_range)
    return (n, "other", is_range)


def _to_mg(value: float, kind: str) -> float | None:
    if kind == "mass_mcg": return value / 1000
    if kind == "mass_mg":  return value
    if kind == "mass_g":   return value * 1000
    return None


def _find_drug_mentions(text: str, table: DrugTable) -> list[dict]:
    drug_spans = _find_drug_spans(text, table)
    breaker_spans = _find_breaker_spans(text, table)
    # Сортированный список «барьеров» = drug + breaker concept
    barriers = sorted([(s, "drug", c) for s, e, c, i, t in drug_spans] +
                      [(s, "break", "_") for s, e in breaker_spans])

    dose_matches = list(DOSE_RE.finditer(text))
    route_matches = list(ROUTE_RE.finditer(text))

    mentions = []
    for idx, (start, end, canon, info, txt) in enumerate(drug_spans):
        # next barrier after this drug
        win_end = len(text)
        for bs, btype, bcanon in barriers:
            if bs > end and bs < win_end:
                # Не считаем self-references (такая же drug-mention позже)
                if bs == start:
                    continue
                win_end = bs
                break
        # Hard cap 100 символов от drug end
        win_end = min(win_end, end + 100)

        bound_dose = None
        for dm in dose_matches:
            if dm.start() < end:
                continue
            if dm.start() >= win_end:
                break
            bound_dose = dm
            break
        bound_route = None
        for rm in route_matches:
            if rm.start() < end:
                continue
            if rm.start() >= win_end:
                break
            bound_route = rm
            break

        m = {
            "canon": canon, "info": info, "match_text": txt,
            "pos": start, "win_end": win_end,
            "window": text[start:win_end + 50],
            "dose_value": None, "dose_kind": None, "dose_raw": None, "dose_is_range": False,
            "route": None, "route_raw": None,
        }
        if bound_dose:
            v, k, r = _extract_dose_value(bound_dose.group("num"), bound_dose.group("unit"))
            m["dose_value"] = v
            m["dose_kind"] = k
            m["dose_raw"] = bound_dose.group(0)
            m["dose_is_range"] = r
        if bound_route:
            m["route"] = normalize_route(bound_route.group(0))
            m["route_raw"] = bound_route.group(0)
        mentions.append(m)
    return mentions


# =============================================================================
# Проверка диапазонов
# =============================================================================

def _ranges_match_route(allowed: str | None, actual: str | None) -> bool:
    if not allowed:
        return True  # диапазон без ограничения route
    if actual is None:
        return True  # путь не указан — не штрафуем
    return any(r == actual for r in allowed.split("|"))


def _check_dose_for_indication(canon: str, info: dict, mention: dict, prompt_lc: str) -> Issue | None:
    """Validate dose against per-indication ranges. Returns Issue or None."""
    if mention["dose_value"] is None:
        return None
    kind = mention["dose_kind"]
    val = mention["dose_value"]
    full_context = (prompt_lc + " " + mention["window"]).lower()

    # Per-kg, concentration, rate — не проверяем абсолютный диапазон
    if kind in ("per_kg", "concentration", "rate"):
        return None

    # Сначала absolute_floor / absolute_ceiling
    val_mg = _to_mg(val, kind)
    if val_mg is not None:
        floor_mg = info.get("absolute_floor_mg")
        if "absolute_floor_mcg" in info and floor_mg is None:
            floor_mg = info["absolute_floor_mcg"] / 1000
        if "absolute_floor_g" in info and floor_mg is None:
            floor_mg = info["absolute_floor_g"] * 1000
        ceiling_mg = info.get("absolute_ceiling_mg")
        if "absolute_ceiling_mcg" in info and ceiling_mg is None:
            ceiling_mg = info["absolute_ceiling_mcg"] / 1000
        if "absolute_ceiling_g" in info and ceiling_mg is None:
            ceiling_mg = info["absolute_ceiling_g"] * 1000

        if floor_mg is not None and val_mg < floor_mg * 0.99:
            note = info.get("absolute_floor_note") or "ниже абсолютного минимума"
            return Issue(canon, CRITICAL,
                         f"доза {mention['dose_raw']} ({val_mg:.3g} мг) НИЖЕ абсолютного минимума ({floor_mg} мг). {note}",
                         span=(mention["pos"], mention["win_end"]))
        if ceiling_mg is not None and val_mg > ceiling_mg * 1.01:
            return Issue(canon, CRITICAL,
                         f"доза {mention['dose_raw']} ({val_mg:.3g} мг) ВЫШЕ абсолютного максимума ({ceiling_mg} мг)",
                         span=(mention["pos"], mention["win_end"]))

    # Per-indication: пробуем все диапазоны, если хоть один подходит — OK
    indications = info.get("doses_by_indication", {})
    matching_ranges = []  # все диапазоны (route+context подходят)
    fallback_ranges = []  # диапазоны без context_pattern или с подходящим route только
    for ind_name, rng_list in indications.items():
        for rng in rng_list:
            route_ok = _ranges_match_route(rng.get("route"), mention["route"])
            if not route_ok:
                continue
            ctx_pat = rng.get("context_pattern")
            ctx_ok = True if not ctx_pat else bool(re.search(ctx_pat, full_context, re.IGNORECASE))
            if ctx_ok:
                matching_ranges.append((ind_name, rng))
            else:
                fallback_ranges.append((ind_name, rng))

    # Берём matching, если есть; иначе fallback
    candidates = matching_ranges if matching_ranges else fallback_ranges
    if not candidates:
        return None  # нет подходящего диапазона

    # Получаем mg-диапазоны кандидатов
    def _range_to_mg(rng: dict) -> tuple[float | None, float | None, str]:
        if "min_mg" in rng:
            return (rng["min_mg"], rng["max_mg"], rng.get("note", ""))
        if "min_mcg" in rng:
            return (rng["min_mcg"] / 1000, rng["max_mcg"] / 1000, rng.get("note", ""))
        if "min_g" in rng:
            return (rng["min_g"] * 1000, rng["max_g"] * 1000, rng.get("note", ""))
        if "min_ml" in rng and kind == "volume_ml":
            return (rng["min_ml"], rng["max_ml"], rng.get("note", ""))
        # per-kg/per-min/etc — не покрываем здесь
        return (None, None, rng.get("note", ""))

    if val_mg is not None:
        # Хотя бы один диапазон должен покрывать
        for ind, rng in candidates:
            tol = rng.get("tolerance", 1.1)
            lo, hi, note = _range_to_mg(rng)
            if lo is None:
                continue
            if lo / tol <= val_mg <= hi * tol:
                return None  # подходит
        # Не подошло ни к одному
        # Берём первый matching range как «ожидаемый»
        ind, rng = candidates[0]
        tol = rng.get("tolerance", 1.1)
        lo, hi, note = _range_to_mg(rng)
        if lo is None:
            return None
        if val_mg < lo / tol:
            sev = CRITICAL if val_mg < lo * 0.3 else IMPORTANT
            return Issue(canon, sev,
                         f"доза {mention['dose_raw']} ({val_mg:.3g} мг) НИЖЕ диапазона для «{ind}» (норма {lo}-{hi} мг). {note}",
                         span=(mention["pos"], mention["win_end"]))
        if val_mg > hi * tol:
            sev = IMPORTANT if val_mg < hi * 3 else CRITICAL
            return Issue(canon, sev,
                         f"доза {mention['dose_raw']} ({val_mg:.3g} мг) ВЫШЕ диапазона для «{ind}» (норма {lo}-{hi} мг). {note}",
                         span=(mention["pos"], mention["win_end"]))
    elif kind == "volume_ml":
        for ind, rng in candidates:
            if "min_ml" in rng and rng["min_ml"] * 0.9 <= val <= rng["max_ml"] * 1.5:
                return None
        # Минимально проверим первый
        ind, rng = candidates[0]
        if "min_ml" in rng:
            if val < rng["min_ml"] * 0.5:
                return Issue(canon, MINOR,
                             f"объём {mention['dose_raw']} ниже типичного для «{ind}»",
                             span=(mention["pos"], mention["win_end"]))
            if val > rng["max_ml"] * 2:
                return Issue(canon, MINOR,
                             f"объём {mention['dose_raw']} выше типичного для «{ind}»",
                             span=(mention["pos"], mention["win_end"]))
    return None


def _check_forbidden_indications(canon: str, info: dict, response: str) -> Issue | None:
    pat = info.get("forbidden_indications_pattern")
    if not pat:
        return None
    m = re.search(pat, response, re.IGNORECASE)
    if not m:
        return None
    sev = info.get("forbidden_severity", IMPORTANT)
    msg = info.get("forbidden_msg", "Препарат применён в неправильном контексте")
    return Issue(canon, sev,
                 f"{msg} (фрагмент: «{m.group(0)[:80]}»)",
                 span=m.span())


def _check_forbidden_routes(canon: str, info: dict, mention: dict) -> Issue | None:
    forbidden = info.get("forbidden_routes_strict", [])
    if not forbidden:
        return None
    if mention.get("route") in forbidden:
        return Issue(canon, IMPORTANT,
                     f"путь введения {mention['route']} запрещён для {canon} (доказательная база только IV/IO)",
                     span=(mention["pos"], mention["win_end"]))
    return None


def _check_forbidden_in_text(canon: str, info: dict, response: str) -> Issue | None:
    pat = info.get("forbidden_pattern_in_text")
    if not pat:
        return None
    m = re.search(pat, response, re.IGNORECASE)
    if not m:
        return None
    return Issue(canon, CRITICAL,
                 f"Запрещённое сочетание для {canon} (фрагмент: «{m.group(0)[:80]}»)",
                 span=m.span())


def _check_forbidden_procedures(response: str, table: DrugTable) -> list[Issue]:
    out = []
    rl = response.lower()
    for proc in table.forbidden_procs:
        m = re.search(proc["trigger_pattern"], rl, re.IGNORECASE)
        if not m:
            continue
        # exception: смягчающий контекст в окне ±150 char
        excp = proc.get("exception_pattern")
        if excp:
            window = rl[max(0, m.start() - 150): m.end() + 150]
            if re.search(excp, window, re.IGNORECASE):
                continue
        out.append(Issue(proc["name"], proc["severity"],
                         f"{proc['msg']} (фрагмент: «{m.group(0)[:80]}»)",
                         span=m.span()))
    return out


def _check_wrong_combinations(response: str, table: DrugTable) -> list[Issue]:
    out = []
    for combo in table.wrong_combos:
        m = re.search(combo["trigger_pattern"], response, re.IGNORECASE)
        if not m:
            continue
        out.append(Issue(combo["name"], combo["severity"],
                         f"{combo['msg']} (фрагмент: «{m.group(0)[:80]}»)",
                         span=m.span()))
    return out


# =============================================================================
# Главный API
# =============================================================================

_TABLE_CACHE: DrugTable | None = None


def get_table() -> DrugTable:
    global _TABLE_CACHE
    if _TABLE_CACHE is None:
        _TABLE_CACHE = DrugTable()
    return _TABLE_CACHE


def reload_table():
    """Перезагрузить таблицу (после правки JSON без перезапуска процесса)."""
    global _TABLE_CACHE
    _TABLE_CACHE = DrugTable()


def validate(prompt: str, response: str) -> list[Issue]:
    """Полная валидация — возвращает все обнаруженные проблемы."""
    table = get_table()
    issues: list[Issue] = []
    pl = prompt.lower()

    # 1. Forbidden indications (per-drug)
    for canon, info in table.drugs.items():
        iss = _check_forbidden_indications(canon, info, response)
        if iss:
            issues.append(iss)
        iss = _check_forbidden_in_text(canon, info, response)
        if iss:
            issues.append(iss)

    # 2. Drug mentions and dose-checks
    mentions = _find_drug_mentions(response, table)
    for m in mentions:
        canon = m["canon"]
        info = m["info"]
        # forbidden routes
        iss = _check_forbidden_routes(canon, info, m)
        if iss:
            issues.append(iss)
        # dose ranges
        iss = _check_dose_for_indication(canon, info, m, pl)
        if iss:
            issues.append(iss)

    # 3. Forbidden field procedures
    issues.extend(_check_forbidden_procedures(response, table))

    # 4. Wrong combinations
    issues.extend(_check_wrong_combinations(response, table))

    return issues


def severity_count(issues: Iterable[Issue]) -> dict[str, int]:
    out = {CRITICAL: 0, IMPORTANT: 0, MINOR: 0}
    for i in issues:
        if i.severity in out:
            out[i.severity] += 1
    return out


# =============================================================================
# Self-tests
# =============================================================================

if __name__ == "__main__":
    import sys
    if sys.platform == "win32":
        try: sys.stdout.reconfigure(encoding="utf-8")
        except: pass

    print(f"Loading drug_table.json...")
    t = get_table()
    print(f"  {len(t.drugs)} препаратов, {len(t.forbidden_procs)} запрещённых процедур, "
          f"{len(t.wrong_combos)} опасных сочетаний.\n")

    cases = [
        # === Форматов и dose-binding ===
        ("h2_real",
         "Снайпер попал в бедро снизу, артериальное кровотечение, боец без сознания",
         "1. НЕМЕДЛЕННО — налоксон 0,5 мг в/м (анальгетик при шоке).",
         1, "налоксон"),
        ("h89_real",
         "Аллергическая реакция на укус пчелы, отёк гортани начинается",
         "Адреналин 1 мкг в/м (если есть аптечка — для анафилаксии).",
         1, "адреналин"),
        ("h103_real",
         "Если нет EZ-IO, можно ли наладить инфузию вручную трепанацией",
         "ДА, ТРЕПАНАЦИЯ — ОБЫЧНОЙ МЕТОД. Делай в области темени.",
         1, "трепанац"),
        ("h105_real",
         "Коникотомия — техника пошагово в полевых",
         "Коникотомия — это фасциотомия на шее, разрез через мембрану.",
         1, "коникот"),

        # === Должно быть OK ===
        ("ok_TXA_2g",
         "Массивное кровотечение",
         "1. CAT. 2. TXA 2 г в/в в первый час.",
         0, None),
        ("ok_TXA_1g_via_im_should_warn",
         "Массивное кровотечение, IV нет",
         "1. CAT. 2. TXA 2 г в/м (по 1 г в каждое бедро).",
         0, None),
        ("ok_адр_05_анафилаксия",
         "Анафилаксия после укуса осы",
         "Адреналин 0.5 мг в/м, повтор каждые 5 мин.",
         0, None),
        ("ok_адр_1мг_СЛР",
         "СЛР, асистолия",
         "Адреналин 1 мг в/в каждые 3-5 мин.",
         0, None),
        ("ok_кетамин_30_iv",
         "Анальгезия при шоке",
         "Кетамин 30 мг в/в медленно за 1 минуту.",
         0, None),
        ("ok_кетамин_50_im",
         "Анальгезия в/м",
         "Кетамин 50 мг в/м (0.5 мг/кг для 70 кг).",
         0, None),
        ("ok_атропин_2_фов",
         "Зарин, миоз, слюнотечение",
         "Атропин 2 мг в/м (1 ATNAA), повтор каждые 5 мин до сухости.",
         0, None),
        ("ok_атропин_1_бради",
         "Брадикардия 35, шок",
         "Атропин 1 мг в/в, повтор каждые 3-5 мин (макс 3 мг).",
         0, None),
        ("ok_моксифлокс_400",
         "Антибиотик при penetrating",
         "Моксифлоксацин 400 мг внутрь 1 раз в сутки.",
         0, None),
        ("ok_эртапенем_1g",
         "Проникающее ранение живота",
         "Эртапенем 1 г в/в 1 раз в сутки.",
         0, None),
        ("ok_парацетамол_1000",
         "Лёгкая боль",
         "Парацетамол 1000 мг каждые 6 часов.",
         0, None),

        # === Опасные ошибки ===
        ("трепанация_в_поле",
         "ЧМТ с признаками вклинения",
         "Сделай декомпрессионную трепанацию иглой 14G в поле.",
         1, "трепанац"),
        ("трепанация_в_стационаре_OK",
         "ЧМТ — что делать",
         "1. ABC. 2. Эвакуация в нейрохирургию — там сделают декомпрессионную трепанацию.",
         0, None),
        ("коникотомия_фасциотомия",
         "Коникотомия — техника",
         "Коникотомия — это фасциотомия на шее.",
         1, "коникотом"),
        ("атропин_05_brady_obsolete",
         "Брадикардия",
         "Атропин 0.5 мг в/в.",
         1, "атропин"),
        ("кетамин_2г",
         "Анальгезия",
         "Кетамин 2 г в/в.",
         1, "кетамин"),
        ("моксифлокс_875",
         "Антибиотик",
         "Моксифлоксацин 875 мг 1 раз в сутки.",
         1, "моксифл"),
        ("сукцинилхолин_гиперкалиемия",
         "RSI у краш-синдрома",
         "Сукцинилхолин 1.5 мг/кг IV при гиперкалиемии — ОК для RSI.",
         2, "сукцин"),  # 1 wrong-combo + ranges OK = 1 issue. Но также forbidden_indications_pattern на сукцинилхолине внутри drug — 2.
        ("TXA_внутримышечно_до_3ч",
         "Кровотечение, IV нет",
         "TXA 2 г в/м (по 1 г в каждое бедро).",
         0, None),

        # === Per-kg, concentration, rate — НЕ должны флагаться ===
        ("per_kg_корр",
         "Доза кетамина у ребёнка",
         "Кетамин 1-2 мг/кг в/в.",
         0, None),
        ("концентрация",
         "Развести TXA",
         "TXA — обычно 2% раствор (20 мг/мл). Доза 1 г = 5 мл.",
         0, None),
        ("rate_адреналин",
         "Инфузия при септическом шоке",
         "Адреналин 5 мкг/мин IV инфузия.",
         0, None),
    ]

    failed = 0
    for label, prompt, response, want_min, want_kw in cases:
        issues = validate(prompt, response)
        ok = len(issues) >= want_min if want_min > 0 else len(issues) == 0
        if ok and want_kw:
            ok = any(want_kw.lower() in str(iss).lower() for iss in issues)
        mark = "[OK]  " if ok else "[FAIL]"
        if not ok:
            failed += 1
        print(f"{mark} {label}: {len(issues)} issues" +
              (f" — {issues[0]}" if issues else ""))
        if not ok:
            print(f"   expected ≥{want_min}" + (f" with «{want_kw}»" if want_kw else ""))
            for iss in issues:
                print(f"   got: {iss}")
    print()
    print("ALL OK" if failed == 0 else f"{failed} CASES FAILED")
