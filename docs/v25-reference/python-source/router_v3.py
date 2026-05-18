"""router_v3.py — V18 расширение router_v2 с доменными LoRA-маршрутизацией и под-категорией.

Возвращает RouteV3:
  - lora_domain: med | cbrn | tac | char | OOS  (для выбора LoRA-адаптера)
  - sub_category: med-tq | med-anaphylaxis | cbrn-fov | cbrn-cyanide | ... (для grammar/KG-query)
  - confidence: 0..1
  - raw: полный RouteResult из router_v2

Маппинг доменов router_v2 → lora_domain:
  med, cbrn → как есть
  tac, comm  → tac
  char, surv, tech → char (общий conversational/устав/выживание/техника)
  oos → OOS (fast-refusal, без LLM)
"""
from __future__ import annotations
import re
from dataclasses import dataclass
from typing import Optional

from router_v2 import route as route_v2, RouteResult


@dataclass
class RouteV3:
    lora_domain: str   # med, cbrn, tac, char, OOS
    sub_category: str  # см. SUB_CATEGORIES
    confidence: float
    raw: RouteResult


_DOMAIN_TO_LORA = {
    "med": "med",
    "cbrn": "cbrn",
    "tac": "tac",
    "comm": "tac",
    "char": "char",
    "surv": "char",
    "tech": "char",
    "oos": "OOS",
}


# Sub-category triggers — используются для выбора KG-запроса и грамматики.
# Структура: (regex|literal, sub_category, weight). Берётся max-weight match.
SUB_CATEGORIES: dict[str, list[tuple[str, str, float]]] = {
    "med": [
        (r"жгут|турникет|tourniquet|\bcat\b|softt", "med-tq", 1.0),
        (r"анафилакс|укус\s+пчел|укус\s+ос|отёк\s+квинке|epipen", "med-anaphylaxis", 1.0),
        (r"пневмоторакс|декомпресс|chest\s+seal|hyfin|asherman|игольн.*грудн", "med-pneumo", 1.0),
        (r"ожог|burn|парклaнд|tbsa|охлажд.*ожог", "med-burn", 1.0),
        (r"ампутац|оторвал.*ног|оторвал.*рук|culт.*коне", "med-amputation", 1.0),
        (r"чмт|gcs|вклинен|трепан|маннитол", "med-tbi", 1.0),
        (r"кетамин|морфин|промедол|фентанил|otfc|обезбол|анальгез", "med-analgesia", 1.0),
        (r"налоксон|narcan|опиоид.*передоз", "med-naloxone", 1.0),
        (r"\btxa\b|транексам|crash-?[23]", "med-txa", 1.5),
        (r"junctional|пах|подмышк|junction", "med-junctional", 1.0),
        (r"триаж|t1|t2|t3|t4|kia|сортировк", "med-triage", 1.0),
        (r"коникотом|интубац|rsi|вдп", "med-airway", 1.0),
        (r"ребен|ребён|педиатр|младенец|новорожд|epipen\s+jr", "med-peds", 1.0),
        (r"глаз|eye|роговиц|конъюнкт|катаракт|линза|инородное\s+тело\s+в\s+глаз", "med-eye", 1.0),
        (r"ез-?io|fast-?1|внутрикост|io\s+доступ", "med-io", 1.0),
        (r"shock|шок\b|fwb|кровь|плазм|гемотрансф", "med-shock", 1.0),
    ],
    "cbrn": [
        (r"зарин|зоман|\bvx\b|фов|нервно-парал|atnaa|пралидоксим|2-?pam", "cbrn-fov", 1.0),
        (r"иприт|mustard|кожно-нарыв|серн.*ипр", "cbrn-mustard", 1.0),
        (r"люизит|лоизит|димеркапрол|унитиол", "cbrn-lewisite", 1.0),
        (r"цианид|hcn|синильн|cyanokit|гидроксокобалам|нитрит\s+натрия|горьк.*миндал", "cbrn-cyanide", 1.0),
        (r"фосген|удушающ.*ов|cocl2", "cbrn-phosgene", 1.0),
        (r"\bхлор\b(?!оф)|cl2", "cbrn-chlorine", 1.0),
        (r"ипп-?(?:8|10|11)|противохимич.*пакет", "cbrn-ipp", 1.0),
        (r"rsdl", "cbrn-rsdl", 1.0),
        (r"облучен|радиац|олб|цистамин|калия\s+йодид|\bki\b|чернобыль", "cbrn-rad", 1.0),
        (r"\bcs\b|\bcr\b|раздраж|слезоточив", "cbrn-irritant", 1.0),
        (r"\bbz\b|инкапацит|психотомим", "cbrn-incapacitant", 1.0),
    ],
    "tac": [
        (r"медэвак|medevac|9-?линей|9-?line|casevac", "tac-medevac", 1.0),
        (r"огнев.*подавл|противодейств|укрытие|марш|приказ", "tac-tactics", 0.8),
        (r"радиообмен|связь|позывн|зашифр|канал", "tac-comms", 1.0),
    ],
    "char": [
        (r"устав|обяз.*солд|обяз.*офиц|обяз.*дежурн|приказ|распоряд|субординац|караул|часов", "char-ustav", 1.5),
        (r"кипячен|вода|съедобн|укрытие|выжива|surviv", "char-survival", 1.0),
        (r"топлив|разморозить|двигатель|техник|btr|бтр|танк|оружие", "char-tech", 1.0),
        (r"стресс|психолог|паник|боев.*стресс|тревог", "char-psy", 1.0),
    ],
    "OOS": [
        (r"анекдот|шутк|погод|рецепт|курс\s+рубл|курс\s+доллар|стих|поэз|астролог|гороскоп|баскет|футбол|хоккей|сериал", "oos-general", 1.0),
    ],
}


def _detect_subcategory(query: str, lora_domain: str) -> str:
    ql = query.lower()
    if lora_domain not in SUB_CATEGORIES:
        return f"{lora_domain}-general"
    best, best_w = f"{lora_domain}-general", 0.0
    for pattern, sub, w in SUB_CATEGORIES[lora_domain]:
        try:
            if re.search(pattern, ql):
                if w > best_w:
                    best, best_w = sub, w
        except re.error:
            if pattern in ql:
                if w > best_w:
                    best, best_w = sub, w
    return best


_TAC_FALLBACK_PATTERNS = [r"медэвак|medevac|9-?линей|9-?line|casevac|радиообмен|позывн"]


def route_v3(query: str) -> RouteV3:
    raw = route_v2(query)
    ql = query.lower()
    # router_v2 fallback для tac-специфичных не-med-фраз
    if raw.domain is None or raw.confidence < 0.3:
        for pat in _TAC_FALLBACK_PATTERNS:
            if re.search(pat, ql):
                sub = _detect_subcategory(query, "tac")
                conf = 0.7 if raw.domain is None else max(raw.confidence, 0.5)
                return RouteV3("tac", sub, conf, raw)
    if raw.domain is None:
        return RouteV3("OOS", "unknown", 0.0, raw)
    lora = _DOMAIN_TO_LORA.get(raw.domain, "char")
    sub = _detect_subcategory(query, lora)
    return RouteV3(lora, sub, raw.confidence, raw)


def domain_for_lora(query: str) -> str:
    return route_v3(query).lora_domain


if __name__ == "__main__":
    import sys
    if sys.platform == "win32":
        try: sys.stdout.reconfigure(encoding="utf-8")
        except: pass

    cases = [
        ("На жгуте 2 часа, можно ли снять", "med", "med-tq"),
        ("Анафилаксия после укуса пчелы", "med", "med-anaphylaxis"),
        ("Напряжённый пневмоторакс — куда колоть", "med", "med-pneumo"),
        ("Ожог 30% TBSA — расчёт инфузии", "med", "med-burn"),
        ("Налоксон 4 мг при шоке — да или нет", "med", "med-naloxone"),
        ("Дозы TXA при ампутации", "med", "med-txa"),
        ("Запах горького миндаля у бойца", "cbrn", "cbrn-cyanide"),
        ("Воздействие зарина — порядок действий", "cbrn", "cbrn-fov"),
        ("Иприт на коже — дегазация", "cbrn", "cbrn-mustard"),
        ("ИПП-11 при хлоре", "cbrn", "cbrn-ipp"),
        ("Вызов медэвака — формат 9-линейки", "tac", "tac-medevac"),
        ("Обязанности дежурного по роте", "char", "char-ustav"),
        ("Кипячение воды — сколько минут", "char", "char-survival"),
        ("Анекдот про начальника", "OOS", "oos-general"),
    ]
    failed = 0
    for q, exp_lora, exp_sub in cases:
        r = route_v3(q)
        ok = (r.lora_domain == exp_lora) and (r.sub_category == exp_sub)
        mark = "[OK]" if ok else "[FAIL]"
        if not ok:
            failed += 1
        print(f"{mark} {r.lora_domain}/{r.sub_category} (exp {exp_lora}/{exp_sub}) conf={r.confidence:.2f} | {q[:50]}")
    print(f"\n{len(cases)-failed}/{len(cases)} passed")
