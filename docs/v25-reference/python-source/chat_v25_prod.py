#!/usr/bin/env python3
"""
chat_v25_prod.py — PROD-обёртка над v25 GGUF + RAG, с интегрированным
post_output_guard и автообнулением контекста.

Отличия от chat_v25.py:
  1. guard()  — post_output_guard.blocking подключён (фикс: V25_FINAL_REPORT
                обещал "PROD-кандидат с blocking guard", но chat_v25.py guard
                не вызывал; все critical/truncated/fabricated/leak проходили
                к пользователю).
  2. auto-reset истории — каждые AUTO_RESET_TURNS (по умолчанию 10) ходов
                и/или при превышении CTX_SOFT_LIMIT токенов в склейке.
  3. graceful shutdown — закрывает SQLite-соединение и освобождает llama_cpp.
  4. cleanup при /sft/--/final — del старой Llama + gc.collect (фикс утечки
                VRAM при переключении ckpt).
  5. domain-tag вычисляется через rag_v5.detect_query_domain и передаётся
                в guard для domain-leak проверки.
  6. история retrieval-склейки — берём последние 2 user-запроса, а не только
                один (фикс: chat_v25.py терял середину контекста при 3+ turn).
  7. структурное логирование (sessions/*.jsonl) — каждый turn пишется в JSONL
                для аудита и retrain-bank.

Запуск:
    python chat_v25_prod.py
    python chat_v25_prod.py --auto-reset 8 --topk 7
    python chat_v25_prod.py --no-rag
    python chat_v25_prod.py --no-guard       # снять guard (debug only)
    python chat_v25_prod.py --sft            # SFT-only ckpt
"""
from __future__ import annotations
import importlib, sys, os, time, argparse, gc, json
from datetime import datetime
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

try:
    from post_output_guard import guard as _guard
    GUARD_AVAILABLE = True
except Exception as e:
    print(f"[WARN] post_output_guard недоступен: {e} — guard будет выключен.")
    GUARD_AVAILABLE = False
    def _guard(prompt, response, **kw):
        return response, "pass", []

BASE = Path(__file__).resolve().parent
GGUF_SFT = BASE / "ModelTununig" / "field-medic-lora" / "checkpoints_qwen3_v25_sft" / "qwen3-4b-soldier-v25-sft-Q4_K_M.gguf"
GGUF_FINAL = BASE / "ModelTununig" / "field-medic-lora" / "checkpoints_qwen3_v25" / "qwen3-4b-soldier-v25-Q4_K_M.gguf"
DB = BASE / "RagSystem" / "field-medic-rag" / "knowledge_v5.db"
EMB_MODEL = "intfloat/multilingual-e5-small"
SESSION_LOG_DIR = BASE / "logs_sessions"
SESSION_LOG_DIR.mkdir(exist_ok=True)

ap = argparse.ArgumentParser()
ap.add_argument("--topk", type=int, default=5)
ap.add_argument("--no-rag", action="store_true")
ap.add_argument("--no-guard", action="store_true",
                help="не вызывать post_output_guard (DEBUG only — не для PROD)")
ap.add_argument("--guard-mode", choices=["blocking", "rewrite", "logging"], default="blocking",
                help="режим guard: blocking (PROD), rewrite (rewrite-only), logging (только лог)")
ap.add_argument("--max-history", type=int, default=6,
                help="окно истории для prompt (default 6 turns)")
ap.add_argument("--auto-reset", type=int, default=10,
                help="auto-reset истории каждые N ходов (0 = выкл)")
ap.add_argument("--ctx-soft-limit", type=int, default=10000,
                help="soft-лимит токенов prompt; при превышении — auto-reset")
ap.add_argument("--temp", type=float, default=0.3)
ap.add_argument("--sft", action="store_true", help="использовать SFT-only checkpoint")
ap.add_argument("--gguf", type=str, default=None, help="явно указать путь к GGUF")
ap.add_argument("--n-ctx", type=int, default=16384, help="n_ctx Llama (умолч. 16384)")
ap.add_argument("--n-gpu-layers", type=int, default=99)
args = ap.parse_args()

AUTO_RESET_TURNS = max(0, args.auto_reset)
CTX_SOFT_LIMIT = max(1024, args.ctx_soft_limit)

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
    print("Сначала собери: 3a_convert_to_gguf_qwen3_v25_sft.py или 3b_convert_to_gguf_qwen3_v25.py")
    sys.exit(1)

print("=" * 60)
print("  ПОЛЕВОЙ ПОМОЩНИК БОЙЦА ВС РФ — v25 PROD")
print("=" * 60)
print(f"GGUF       : {gguf_path.name}")
print(f"RAG        : {DB.name}  ({'OFF' if args.no_rag else f'top-{args.topk}'})")
guard_label = "OFF" if (args.no_guard or not GUARD_AVAILABLE) else args.guard_mode
print(f"Guard      : {guard_label}")
print(f"Auto-reset : каждые {AUTO_RESET_TURNS} ходов" if AUTO_RESET_TURNS else "Auto-reset : выкл.")
print(f"Ctx-soft   : {CTX_SOFT_LIMIT} токенов (приблизительно)")
print("Загрузка...")

emb_tok = AutoTokenizer.from_pretrained(EMB_MODEL)
emb_model = AutoModel.from_pretrained(EMB_MODEL); emb_model.eval()
print("  [OK] Embedding")

llm = Llama(model_path=str(gguf_path), n_ctx=args.n_ctx, n_threads=8,
            n_gpu_layers=args.n_gpu_layers, verbose=False)
print("  [OK] LLM")

chunks, vec_stack, conn = [], None, None
if not args.no_rag and DB.exists():
    chunks, vec_stack, conn = rag_v5.load_db(DB)
    print(f"  [OK] База ({len(chunks)} чанков)")
elif not DB.exists():
    print(f"  [WARN] RAG БД не найдена ({DB}) — продолжаю без RAG")

print()
print("Команды: /reset | /norag | /rag | /sft | /final | /stats | q")
print("-" * 60)

SESSION_ID = datetime.utcnow().strftime("%Y%m%d_%H%M%S")
session_log_path = SESSION_LOG_DIR / f"session_{SESSION_ID}.jsonl"


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


def approx_tokens(text: str) -> int:
    """Приблизительный счётчик токенов (4 chars ≈ 1 token для RU/EN mix)."""
    return max(1, len(text) // 4)


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
    try:
        del llm
    except Exception:
        pass
    gc.collect()
    if torch.cuda.is_available():
        torch.cuda.empty_cache()
    llm = Llama(model_path=str(new_path), n_ctx=args.n_ctx, n_threads=8,
                n_gpu_layers=args.n_gpu_layers, verbose=False)
    gguf_path = new_path


def log_turn(rec: dict):
    try:
        with open(session_log_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    except Exception:
        pass


def shutdown():
    print("\nЗакрытие ресурсов...")
    try:
        if conn is not None:
            conn.close()
    except Exception:
        pass
    try:
        del_llm = globals().get("llm")
        if del_llm is not None:
            del globals()["llm"]
    except Exception:
        pass
    gc.collect()
    print(f"Сессия: {session_log_path.name}")


history = []
rag_on = not args.no_rag and bool(chunks)
guard_on = (not args.no_guard) and GUARD_AVAILABLE
turn_count = 0
total_turns = 0      # с начала программы (не сбрасывается /reset'ом)
blocked_count = 0


try:
    while True:
        try:
            print()
            tags = []
            tags.append("rag" if rag_on else "no-rag")
            if guard_on:
                tags.append("guard")
            label = f"{'/'.join(tags)} [{turn_count}/{AUTO_RESET_TURNS or '∞'}]>"
            query = input(f"\033[1;32m{label} \033[0m").strip()
        except (EOFError, KeyboardInterrupt):
            break

        if not query or query.lower() in ("q", "quit", "exit", "выход"):
            break

        if query.lower() in ("/reset", "/clear", "/new"):
            history = []
            turn_count = 0
            print("\033[33mИстория очищена.\033[0m")
            continue
        if query.lower() == "/norag":
            rag_on = False; print("\033[33mRAG выключен.\033[0m"); continue
        if query.lower() == "/rag":
            rag_on = bool(chunks); print(f"\033[33mRAG {'включён' if rag_on else 'недоступен'}.\033[0m"); continue
        if query.lower() == "/sft":
            reload_llm(GGUF_SFT); continue
        if query.lower() == "/final":
            reload_llm(GGUF_FINAL); continue
        if query.lower() == "/stats":
            print(f"\033[33mTotal turns: {total_turns}, blocked: {blocked_count}, "
                  f"history len: {len(history)}, log: {session_log_path.name}\033[0m")
            continue

        t0 = time.time()

        # история для retrieval: склейка последних 2 user-запросов (если есть)
        prev_users = [u for (u, _) in history[-2:]]
        search_q = " ".join(prev_users + [query])

        if rag_on and chunks:
            qv = embed_query(search_q)
            ctx = rag_v5.hybrid_retrieve(search_q, qv, chunks, vec_stack, conn, top_k=args.topk)
        else:
            ctx = []
        prompt = build_prompt(query, ctx, history)

        # Soft-limit по токенам: до генерации
        if approx_tokens(prompt) > CTX_SOFT_LIMIT and history:
            print(f"\033[33m[auto-reset] prompt ≈{approx_tokens(prompt)} токенов > {CTX_SOFT_LIMIT} — обнуляю историю.\033[0m")
            history = []
            turn_count = 0
            prompt = build_prompt(query, ctx, history)

        answer = generate(prompt)

        # post-output guard
        action = "pass"
        issues = []
        if guard_on:
            qd_tag = rag_v5.detect_query_domain(query) or "unknown"
            try:
                answer, action, issues = _guard(query, answer, mode=args.guard_mode,
                                                rag_chunks=ctx, response_tag=qd_tag,
                                                log_file=str(SESSION_LOG_DIR / "guard.jsonl"))
            except Exception as e:
                print(f"\033[31m[guard error] {e}\033[0m")
                action = "guard-error"

        if action == "blocked":
            blocked_count += 1

        history.append((query, answer))
        turn_count += 1
        total_turns += 1
        elapsed = time.time() - t0

        # log
        log_turn({
            "ts": datetime.utcnow().isoformat() + "Z",
            "turn": total_turns,
            "query": query[:400],
            "answer": answer[:1500],
            "action": action,
            "issues": [str(i) for i in issues],
            "ctx_n": len(ctx),
            "elapsed_s": round(elapsed, 2),
        })

        print()
        print(f"\033[1;36m{'─' * 60}\033[0m")
        print(f"\033[1;37m{answer}\033[0m")
        print(f"\033[1;36m{'─' * 60}\033[0m")

        meta = [f"{elapsed:.1f}с"]
        if ctx:
            kw = sum(1 for c in ctx if c.get("from_kw"))
            top = ctx[0]
            meta.append(f"RAG: {len(ctx)} чанков (kw:{kw}) top={top['score']:.3f} {top['source'][:30]}")
        else:
            meta.append("RAG: off")
        if guard_on:
            meta.append(f"guard:{action}")
        print(f"\033[90m[{' | '.join(meta)}]\033[0m")

        # auto-reset по числу ходов — ПОСЛЕ ответа, чтобы текущий контекст работал
        if AUTO_RESET_TURNS and turn_count >= AUTO_RESET_TURNS:
            print(f"\033[33m[auto-reset] {AUTO_RESET_TURNS} ходов достигнуто — обнуляю историю.\033[0m")
            history = []
            turn_count = 0

finally:
    shutdown()
