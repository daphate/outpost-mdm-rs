#!/usr/bin/env python3
"""
chat_v25.py — полевой помощник бойцу ВС РФ, версия v25 + RAG.

Связка:
  модель = qwen3-4b-soldier-v25-Q4_K_M.gguf
           (LoRA-обученный Qwen3-4B-Instruct-2507; SFT V25 + DPO V25)
  RAG    = knowledge_v5.db (как в v13)
  логика = rag_v5.py (vector + keyword + diversity + [Фрагмент N])

V25 отличия от V13/V23 (V25_PLAN.md):
  - модель отвечает в человеческом стиле, 150-300 слов
  - identity: "помощник бойцу ВС РФ", не "врач"
  - русский язык без EN-терминов
  - расширенные домены: TAC/SURV/TECH/CHAR/OOS

Запуск:
  python chat_v25.py
  python chat_v25.py --topk 7
  python chat_v25.py --no-rag
  python chat_v25.py --sft       # SFT-only checkpoint (без DPO)
"""
import importlib, sys, os, time, argparse
from pathlib import Path

if sys.platform == "win32":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    os.system("chcp 65001 >nul 2>&1")

_orig = importlib.util.find_spec
importlib.util.find_spec = lambda n, *a, **kw: None if n == "torchvision" else _orig(n, *a, **kw)

import numpy as np
import torch
from transformers import AutoTokenizer, AutoModel
from llama_cpp import Llama

import rag_v5
try:
    from response_cleaner import clean_response
except Exception:
    def clean_response(t): return t.strip()

BASE = Path(__file__).resolve().parent
GGUF_SFT = BASE / "ModelTununig" / "field-medic-lora" / "checkpoints_qwen3_v25_sft" / "qwen3-4b-soldier-v25-sft-Q4_K_M.gguf"
GGUF_FINAL = BASE / "ModelTununig" / "field-medic-lora" / "checkpoints_qwen3_v25" / "qwen3-4b-soldier-v25-Q4_K_M.gguf"
DB = BASE / "RagSystem" / "field-medic-rag" / "knowledge_v5.db"
EMB_MODEL = "intfloat/multilingual-e5-small"

ap = argparse.ArgumentParser()
ap.add_argument("--topk", type=int, default=5)
ap.add_argument("--no-rag", action="store_true")
ap.add_argument("--max-history", type=int, default=6)
ap.add_argument("--temp", type=float, default=0.3)
ap.add_argument("--sft", action="store_true", help="использовать SFT-only checkpoint")
ap.add_argument("--gguf", type=str, default=None, help="явно указать путь к GGUF")
args = ap.parse_args()

if args.gguf:
    gguf_path = Path(args.gguf)
elif args.sft:
    gguf_path = GGUF_SFT
elif GGUF_FINAL.exists():
    gguf_path = GGUF_FINAL
else:
    gguf_path = GGUF_SFT

if not gguf_path.exists():
    print(f"[ERR] GGUF не найден: {gguf_path}")
    print(f"Сначала собери: 3a_convert_to_gguf_qwen3_v25_sft.py или 3b_convert_to_gguf_qwen3_v25.py")
    sys.exit(1)

print("=" * 60)
print("  ПОЛЕВОЙ ПОМОЩНИК БОЙЦА ВС РФ — v25 + RAG")
print("=" * 60)
print(f"GGUF : {gguf_path.name}")
print(f"RAG  : {DB.name}  ({'OFF' if args.no_rag else f'top-{args.topk}'})")
print("Загрузка...")

emb_tok = AutoTokenizer.from_pretrained(EMB_MODEL)
emb_model = AutoModel.from_pretrained(EMB_MODEL); emb_model.eval()
print("  [OK] Embedding")

llm = Llama(model_path=str(gguf_path), n_ctx=16384, n_threads=8, n_gpu_layers=99, verbose=False)
print("  [OK] LLM")

if not args.no_rag and DB.exists():
    chunks, vec_stack, conn = rag_v5.load_db(DB)
    print(f"  [OK] База ({len(chunks)} чанков)")
else:
    if not DB.exists():
        print(f"  [WARN] RAG БД не найдена ({DB}) — продолжаю без RAG")
    chunks, vec_stack, conn = [], None, None

print()
print("Команды: /reset | /norag | /rag | /sft | /final | q")
print("-" * 60)


def embed_query(text: str) -> np.ndarray:
    enc = emb_tok(["query: " + text], padding=True, truncation=True, max_length=512, return_tensors="pt")
    with torch.no_grad():
        out = emb_model(**enc)
    mask = enc["attention_mask"].unsqueeze(-1).expand(out.last_hidden_state.size()).float()
    e = torch.sum(out.last_hidden_state * mask, 1) / torch.clamp(mask.sum(1), min=1e-9)
    e = torch.nn.functional.normalize(e, p=2, dim=1)
    return e.numpy()[0].astype(np.float32)


def build_prompt(query: str, ctx_chunks, history) -> str:
    if ctx_chunks:
        qd = rag_v5.detect_query_domain(query)
        sys_txt = rag_v5.build_system(ctx_chunks, query_domain=qd)
    else:
        sys_txt = (
            "Ты — полевой помощник бойцу ВС РФ. Помогаешь по медпомощи раненому, "
            "действиям под огнём, ремонту техники, выживанию, уставу и связи. "
            "Отвечай по-русски, развёрнуто (150-300 слов), без английских терминов. "
            "Если вопрос вне твоей зоны — скажи прямо."
        )
    parts = [f"<|im_start|>system\n{sys_txt}<|im_end|>"]
    for u, a in history[-args.max_history:]:
        parts.append(f"<|im_start|>user\n{u}<|im_end|>")
        parts.append(f"<|im_start|>assistant\n{a}<|im_end|>")
    parts.append(f"<|im_start|>user\n{query}<|im_end|>")
    parts.append("<|im_start|>assistant\n")
    return "\n".join(parts)


def generate(prompt: str) -> str:
    out = llm(prompt, max_tokens=2048, temperature=args.temp, top_k=40, top_p=0.9,
              repeat_penalty=1.13, frequency_penalty=0.2, presence_penalty=0.2,
              stop=["<|im_end|>", "<|im_start|>"], echo=False)
    return clean_response(out["choices"][0]["text"])


def reload_llm(new_path: Path):
    global llm, gguf_path
    if not new_path.exists():
        print(f"\033[31m[ERR] не найден: {new_path}\033[0m")
        return
    print(f"\033[33mПерезагрузка LLM {new_path.name}...\033[0m")
    llm = Llama(model_path=str(new_path), n_ctx=16384, n_threads=8, n_gpu_layers=99, verbose=False)
    gguf_path = new_path


history = []
rag_on = not args.no_rag and bool(chunks)

while True:
    try:
        print()
        label = "rag>" if rag_on else "no-rag>"
        query = input(f"\033[1;32m{label} \033[0m").strip()
    except (EOFError, KeyboardInterrupt):
        print("\nВыход.")
        break
    if not query or query.lower() in ("q", "quit", "exit", "выход"):
        print("Выход.")
        break
    if query.lower() in ("/reset", "/clear", "/new"):
        history = []; print("\033[33mИстория очищена.\033[0m"); continue
    if query.lower() == "/norag":
        rag_on = False; print("\033[33mRAG выключен.\033[0m"); continue
    if query.lower() == "/rag":
        rag_on = bool(chunks); print(f"\033[33mRAG {'включён' if rag_on else 'недоступен'}.\033[0m"); continue
    if query.lower() == "/sft":
        reload_llm(GGUF_SFT); continue
    if query.lower() == "/final":
        reload_llm(GGUF_FINAL); continue

    t0 = time.time()
    search_q = query if not history else f"{history[-1][0]} {query}"
    if rag_on and chunks:
        qv = embed_query(search_q)
        ctx = rag_v5.hybrid_retrieve(search_q, qv, chunks, vec_stack, conn, top_k=args.topk)
    else:
        ctx = []
    p = build_prompt(query, ctx, history)
    answer = generate(p)
    history.append((query, answer))
    elapsed = time.time() - t0

    print()
    print(f"\033[1;36m{'─' * 60}\033[0m")
    print(f"\033[1;37m{answer}\033[0m")
    print(f"\033[1;36m{'─' * 60}\033[0m")
    if ctx:
        kw = sum(1 for c in ctx if c.get("from_kw"))
        top = ctx[0]
        print(f"\033[90m[{elapsed:.1f}с | RAG: {len(ctx)} чанков (kw:{kw}) | "
              f"top={top['score']:.3f} {top['source'][:40]}]\033[0m")
    else:
        print(f"\033[90m[{elapsed:.1f}с | RAG: off]\033[0m")
