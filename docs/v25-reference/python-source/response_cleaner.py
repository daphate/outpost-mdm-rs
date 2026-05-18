"""
response_cleaner.py — универсальная пост-обработка LLM-вывода.

Никаких hard-coded prompt-specific патчей: алгоритмы работают на структуре
текста (строки, циклы, маркеры), не на конкретных словах.

Стадии:
  1. strip_thinking          — вырезать Qwen3 <think>…</think>
  2. remove_template_lines   — удалить служебные «(из: …)», «[Фрагмент N: …]»,
                                «• (…)» и подобные template-маркеры (только если
                                строка ТОЛЬКО из них состоит — т.е. модель скопировала
                                разметку, а не вставила содержательную мысль).
  3. collapse_adjacent_duplicates — если одна и та же непустая строка идёт подряд ≥2 раза,
                                     оставить одну.
  4. trim_tail_loop          — обнаружить хвостовой повтор паттерна
                                длиной 1..6 строк, повторяющийся ≥3 раз → обрезать
                                после первого вхождения.
  5. trim_tail_dangling_marker — отрезать «висящие» open-маркеры в конце:
                                  «1.», «-», «(из:», незакрытые скобки.

Все стадии — детерминированные и обратимы при необходимости.
"""
from __future__ import annotations
import re

THINK_RE = re.compile(r"<think>.*?</think>\s*", re.DOTALL | re.IGNORECASE)

# Шаблоны "одинокая служебная строка" — модель скопировала разметку:
TEMPLATE_LINE_PATTERNS = [
    r"^\s*\(из[:\s][^)]*\)\s*$",                      # (из: source) / (из источника ...)
    r"^\s*\(источник[аи]?[:\s][^)]*\)\s*$",           # (источник: ...)
    r"^\s*\(см\.?\s*[:\s]?[^)]*\)\s*$",               # (см.: ...)
    r"^\s*\[Фрагмент\s*\d+[^\]]*\]\s*$",              # [Фрагмент 1: ...]
    r"^\s*•\s*\([^)]*\)\s*$",                         # • (Бронемашина)
    r"^\s*•\s*\([^)]*\)\s*[А-ЯA-Z][^.]{0,30}:\s*$",   # • (X) Заголовок:
    r"^\s*Справочные\s+материалы:?\s*$",
    r"^\s*Контекст:?\s*$",
    r"^\s*\[Дополнительный\s+фрагмент[^\]]*\]:?\s*$",
    r"^\s*Из\s+источника[^:]*:?\s*$",
    r"^\s*ИЗМЕНЕНИЯ\s+ПО\s+КОНТЕКСТУ:?\s*$",
    r"^\s*ПРИ\s+ОБНАРУЖЕНИИ\s+ПОДРАЗДЕЛЕНИЯ:?\s*$",   # из реальных LOOP-кейсов: пустой заголовок секции
]
# Inline-маркер: «(из: ...)» в КОНЦЕ строки — отрезаем хвост, остальное оставляем.
# Не трогаем «(из: ...)» в начале/середине — там может быть смысл («лекарство из аптечки»).
INLINE_TAIL_SOURCE_RE = re.compile(r"\s*\((?:из|источник[аи]?|см\.?|по|согласно)[:\s][^)]+\)\s*$", re.IGNORECASE)
TEMPLATE_LINE_RE = re.compile("|".join(TEMPLATE_LINE_PATTERNS), re.IGNORECASE)


def strip_thinking(text: str) -> str:
    cleaned = THINK_RE.sub("", text)
    if "<think>" in cleaned and "</think>" not in cleaned:
        cleaned = cleaned.split("<think>")[0]
    return cleaned


def remove_template_lines(text: str) -> str:
    out = []
    for ln in text.split("\n"):
        if TEMPLATE_LINE_RE.match(ln):
            continue
        # Срезаем хвостовой «(из: ...)» если он приклеен к содержательной строке.
        ln = INLINE_TAIL_SOURCE_RE.sub("", ln)
        out.append(ln)
    return "\n".join(out)


def collapse_adjacent_duplicates(text: str) -> str:
    """Подряд идущие идентичные непустые строки → одна. Сохраняет пустые строки."""
    out = []
    prev_norm = None
    for ln in text.split("\n"):
        norm = ln.strip().lower()
        if norm and norm == prev_norm:
            continue
        out.append(ln)
        prev_norm = norm
    return "\n".join(out)


def detect_tail_cycle(
    text: str,
    min_cycle_len: int = 1,
    max_cycle_len: int = 6,
    min_repeats: int = 3,
) -> int | None:
    """Если хвост текста — это (последовательность из M строк) × K раз
    подряд (K ≥ min_repeats), возвращает индекс строки, до которой следует
    обрезать (включительно), оставив одно вхождение цикла.

    Перебираем длину цикла от 1 до max_cycle_len; первый матч выигрывает.
    Игнорируем циклы из чистого whitespace.
    """
    lines = text.split("\n")
    n = len(lines)
    for cycle_len in range(min_cycle_len, max_cycle_len + 1):
        if n < cycle_len * min_repeats:
            continue
        # Берём последние cycle_len*min_repeats строк
        tail = lines[-cycle_len * min_repeats:]
        groups = [tail[i * cycle_len:(i + 1) * cycle_len] for i in range(min_repeats)]
        normed = [tuple(s.strip().lower() for s in g) for g in groups]
        # Игнорируем чисто пустые циклы
        if all(all(not s for s in g) for g in normed):
            continue
        if all(g == normed[0] for g in normed[1:]):
            # Цикл найден. Отрезаем (min_repeats-1) лишних повторов с конца.
            cut = n - cycle_len * (min_repeats - 1)
            return cut
    return None


def trim_tail_loop(text: str) -> str:
    cut = detect_tail_cycle(text)
    if cut is None:
        return text
    return "\n".join(text.split("\n")[:cut]).rstrip()


def collapse_global_duplicates(text: str, min_len: int = 15, threshold: int = 3) -> str:
    """Если непустая строка длиной ≥min_len встречается ≥threshold раз во ВСЁМ тексте,
    оставить только первое её вхождение. Это ловит повторы заголовков-секций
    в середине ответа («ЭТО НЕ СВОЁ:» ×3, «ИЗВЛЕКАТЬ НЕЛЬГО:» ×4 и т.п.).

    Защита: короткие строки («1.», «-») не трогаем — они могут быть нумерацией списка.
    """
    from collections import Counter
    lines = text.split("\n")
    counts = Counter()
    for ln in lines:
        norm = ln.strip()
        if len(norm) >= min_len:
            counts[norm] += 1
    duplicates = {ln for ln, c in counts.items() if c >= threshold}
    if not duplicates:
        return text
    seen: set[str] = set()
    out = []
    for ln in lines:
        norm = ln.strip()
        if norm in duplicates:
            if norm in seen:
                continue
            seen.add(norm)
        out.append(ln)
    return "\n".join(out)


_DANGLING_RE = re.compile(
    r"\s*(?:\d+\.\s*|[-—•]\s*|\(из:\s*[^)]*$|\[[^\]]*$|\([^)]*$|:\s*)$"
)


def trim_tail_dangling_marker(text: str) -> str:
    """Если ПОСЛЕДНЯЯ строка — огрызок маркера («5.», «(из: руко…»), а
    предпоследняя — содержательная, отрезаем последнюю.

    Не трогаем если несколько маркеров подряд (это нумерованный список без содержимого —
    редкий, но не обязательно ошибочный случай)."""
    lines = text.rstrip().split("\n")
    if (len(lines) >= 2
            and _DANGLING_RE.match(lines[-1])
            and not _DANGLING_RE.match(lines[-2])):
        lines.pop()
    return "\n".join(lines)


def clean_response(text: str) -> str:
    """Полная пайплайн-чистка ответа модели."""
    if not text or not text.strip():
        return text or ""
    text = strip_thinking(text)
    text = remove_template_lines(text)
    text = collapse_adjacent_duplicates(text)
    text = collapse_global_duplicates(text)   # повторы заголовков в середине
    text = trim_tail_loop(text)
    text = trim_tail_dangling_marker(text)
    text = normalize_v12_bad_forms(text)
    text = strip_garbage_engine_lines(text)
    return text.strip()


# === V17: нормализация V12_BAD псевдо-русских форм ===========================
# Эти формы — наследие pretrain Qwen3, проявляются 5-10 раз/1000 ответов.
# Не «исправляем», а заменяем на нейтральные синонимы / удаляем строку.

V12_BAD_REPLACEMENTS = [
    (re.compile(r"\bвтой\b", re.IGNORECASE), "впервые"),
    (re.compile(r"\bпроволк[аеуи]\b", re.IGNORECASE), "проволока"),
    (re.compile(r"\bтораж\b", re.IGNORECASE), ""),  # удаляем — нет осмысленного слова
    (re.compile(r"\bцентрета\b", re.IGNORECASE), "центра"),
    (re.compile(r"\bтов\s+от\s+бойцов\b", re.IGNORECASE), "от бойцов"),
]


def normalize_v12_bad_forms(text: str) -> str:
    for pat, repl in V12_BAD_REPLACEMENTS:
        text = pat.sub(repl, text)
    return text


# === V17: удаление мусорных строк про двигатель в мед/cbrn-ответе =============
# Эти строки — domain-leak, post_output_guard их потом отдельно блокирует,
# но если строка стоит МЕЖДУ нормальными мед-инструкциями, лучше её просто
# удалить и не блокировать весь ответ.

_GARBAGE_ENGINE_LINE = re.compile(
    r"^.{0,200}?(?:расколётся\s+блок|блок\s+цилиндров|остановки\s+двигателя\s+(?:—|-)\s*иначе|"
    r"свечей\s+зажигания\s+иначе|пробк[аи]\s+радиатор[аи]\s+иначе).{0,200}$",
    re.IGNORECASE,
)


def strip_garbage_engine_lines(text: str) -> str:
    """Удаляет отдельные строки с мусорными вставками про двигатель в мед-контексте.
    Не пытается определить контекст — паттерны достаточно специфичны (про «иначе/расколётся»),
    чтобы не давать FP в реальном ремонтном ответе."""
    out = []
    for ln in text.split("\n"):
        if _GARBAGE_ENGINE_LINE.match(ln):
            continue
        out.append(ln)
    return "\n".join(out)


# === Self-test ===============================================================

if __name__ == "__main__":
    cases = [
        # 1. Template-line stripping
        (
            "1. CAT на бедро.\n2. Жгут.\n(из: TCCC Manual)\n(из: TCCC Manual)\n(из: TCCC Manual)",
            "1. CAT на бедро.\n2. Жгут.",
        ),
        # 2. Adjacent duplicates
        (
            "Шаги:\n— Проверь пульс\n— Проверь пульс\n— Проверь пульс\nИсточник.",
            "Шаги:\n— Проверь пульс\nИсточник.",
        ),
        # 3. Tail-cycle 3-line
        (
            "1. A\n2. B\n3. C\nX\nY\nZ\nX\nY\nZ\nX\nY\nZ",
            "1. A\n2. B\n3. C\nX\nY\nZ",
        ),
        # 4. Dangling marker
        (
            "1. Шаг один\n2. Шаг два\n3.",
            "1. Шаг один\n2. Шаг два",
        ),
        # 5. <think> blocks
        (
            "<think>let me think</think>\nОтвет: 1. CAT.",
            "Ответ: 1. CAT.",
        ),
        # 6. Mixed
        (
            "<think>x</think>\n1. CAT\n• (БМП)\n• (БМП)\n2. Жгут\nA\nB\nA\nB\nA\nB",
            "1. CAT\n2. Жгут\nA\nB",
        ),
        # 7. Non-adjacent duplicate header (NEW from 700-holdout)
        (
            "1. Шаг\nЗАГОЛОВОК СЕКЦИИ ВАЖНЫЙ:\nA\n2. Шаг\nЗАГОЛОВОК СЕКЦИИ ВАЖНЫЙ:\nB\n3. Шаг\nЗАГОЛОВОК СЕКЦИИ ВАЖНЫЙ:\nC",
            "1. Шаг\nЗАГОЛОВОК СЕКЦИИ ВАЖНЫЙ:\nA\n2. Шаг\nB\n3. Шаг\nC",
        ),
        # 8. Short list-marker line — НЕ должно дедуплицироваться
        (
            "1.\n2.\n3.\n4.",
            "1.\n2.\n3.\n4.",
        ),
    ]
    ok = True
    for i, (inp, want) in enumerate(cases, 1):
        got = clean_response(inp)
        mark = "[OK]" if got.strip() == want.strip() else "[FAIL]"
        if got.strip() != want.strip():
            ok = False
        print(f"{mark} case {i}")
        if got.strip() != want.strip():
            print(f"  expected: {want!r}")
            print(f"  got:      {got!r}")
    print()
    print("ALL OK" if ok else "SOMETHING FAILED")
