"""export_triggers_for_android.py — выгрузить триггеры/правила V25 в JSON для
Kotlin-портирования. Запускать из D:\\Soldier\\.

На выходе:
  V25_PROD_PACKAGE/assets_for_android/triggers_v25.json
  V25_PROD_PACKAGE/assets_for_android/drug_table.json (копия)
"""
import json, shutil
from pathlib import Path

import rag_v5

OUT = Path(__file__).resolve().parent / "V25_PROD_PACKAGE" / "assets_for_android"
OUT.mkdir(parents=True, exist_ok=True)

bundle = {
    "version": "v25",
    "DOMAIN_TRIGGERS":      rag_v5.DOMAIN_TRIGGERS,
    "DOMAIN_KILL_PENALTY":  rag_v5.DOMAIN_KILL_PENALTY,
    "SYMPTOM_TO_TREATMENT": rag_v5.SYMPTOM_TO_TREATMENT,
    "DOC_TYPE_BOOST":       rag_v5.DOC_TYPE_BOOST,
    "SAFETY_PINS":          rag_v5.SAFETY_PINS,
    "PIN_ACTIONS":          rag_v5.PIN_ACTIONS,
    "MAX_PER_SOURCE":       rag_v5.MAX_PER_SOURCE,
    "SYSTEM_BASE":          rag_v5.SYSTEM_BASE,
    "RAG_INSTRUCTION":      rag_v5.RAG_INSTRUCTION,
    "OOS_STRICT_DIRECTIVE": rag_v5.OOS_STRICT_DIRECTIVE,
    "NO_CONTEXT_HINT":      rag_v5.NO_CONTEXT_HINT,
}

with open(OUT / "triggers_v25.json", "w", encoding="utf-8") as f:
    json.dump(bundle, f, ensure_ascii=False, indent=2)

src_drug = Path(__file__).resolve().parent / "drug_table.json"
if src_drug.exists():
    shutil.copy2(src_drug, OUT / "drug_table.json")

print(f"[OK] triggers_v25.json — {(OUT / 'triggers_v25.json').stat().st_size} bytes")
print(f"[OK] drug_table.json  — копия в {OUT}")
print(f"\nГотово для Android assets/: {OUT}")
