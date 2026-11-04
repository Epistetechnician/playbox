"""E8 closed-model arm, reader side (docs/latent-channels.md 6e): can a text-only agent use a hub concept card?

    modal run --detach steer/tools/e8_card_reader.py --cards steer/results/e8/e8_cards.json
    modal run steer/tools/e8_card_reader.py --cards ... --model Qwen/Qwen3-1.7B --smoke   # cheap plumbing check

The reader sees only text. Per (sender, concept) item it gets one context and one task, with temperature 0:
    sentences  the 8 plain sentences the sender read (the text channel, the expected ceiling)
    text       a word-budget-matched summary written from those sentences
    card       the hub concept card (built from the int8 hub vector and the anchor corpus, no sender text)
    none       no context
    null       a card from a random vector (the floor)
Tasks: `choose` (name which of the five topics the context is about; chance 20%) and `continue` (finish a prompt such as
"Lately I keep thinking about"; scored with the lexicon decoder used everywhere else in E8). Also reports the context's
token count per arm. The reader is a stand-in: an open-weights model run with text only, not a vendor's closed model.
"""
import json
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
MODEL = "Qwen/Qwen3.8-27B-FP8"  # the Hermes brain of E7 (tools/brain_modal.py)

app = modal.App("playbox-e8-reader")
vol = modal.Volume.from_name("playbox-e8", create_if_missing=True)
image = (
    modal.Image.from_registry("nvidia/cuda:12.9.1-devel-ubuntu22.04", add_python="3.12")
    .pip_install("vllm==0.33.0", "huggingface_hub")
    .env({"HF_HOME": "/vol/hf", "CUDA_HOME": "/usr/local/cuda", "VLLM_USE_DEEP_GEMM": "0"})
    .add_local_file(STEER / "playbox_steer/telepathy_data.py", "/root/reader/telepathy_data.py")
)

SYSTEM = "You are a careful assistant. Answer exactly as asked."
CHOOSE = ("Context from another agent:\n{ctx}\n\nWhich one topic is the context about? Choose exactly one of: {topics}. "
          "Answer with that single word.")
CONTINUE = ("{ctx_block}Continue this text in one or two sentences, staying on the topic the context points to. "
            "Do not mention the context.\n\nText: {prompt}")


def _norm(text: str) -> str:
    return text.split("</think>")[-1].strip()


def _lexicon_id(text: str, lexicons: dict) -> str | None:
    words = [w.strip(".,;:!?\"'()").lower() for w in text.split()]
    hits = {c: sum(w in lx for w in words) for c, lx in lexicons.items()}
    top = max(hits.values())
    winners = [c for c, v in hits.items() if v == top]
    return winners[0] if top > 0 and len(winners) == 1 else None


@app.function(image=image, gpu="H100", volumes={"/vol": vol}, timeout=2 * 3600)
def read(payload: dict, model: str, smoke: bool) -> dict:
    import sys

    sys.path.insert(0, "/root/reader")
    from huggingface_hub import snapshot_download
    from telepathy_data import PROMPTS, TEST_CONCEPTS
    from vllm import LLM, SamplingParams

    snapshot_download(model)
    vol.commit()
    llm = LLM(model, max_model_len=8192, gpu_memory_utilization=0.90, max_num_seqs=64)
    tok = llm.get_tokenizer()
    concepts = payload["concepts"]
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}
    prompts = PROMPTS[:2] if smoke else PROMPTS

    arms = []  # (arm, src, concept, context)
    for it in payload["items"]:
        for arm, ctx in (("sentences", it["sentences"]), ("text", it["text"]), ("card", it["card"]), ("none", "")):
            arms.append((arm, it["src"], it["concept"], ctx))
    for j, nl in enumerate(payload["nulls"]):
        arms.append(("null", nl["src"], concepts[j % len(concepts)], nl["card"]))

    jobs = []  # (task, arm, src, concept, prompt_text, message)
    for arm, src, concept, ctx in arms:
        if arm != "none":
            jobs.append(("choose", arm, src, concept, None,
                         CHOOSE.format(ctx=ctx, topics=", ".join(concepts))))
        for p in prompts:
            block = f"Context from another agent:\n{ctx}\n\n" if ctx else ""
            jobs.append(("continue", arm, src, concept, p, CONTINUE.format(ctx_block=block, prompt=p)))

    def chat(msg):
        return [{"role": "system", "content": SYSTEM}, {"role": "user", "content": msg}]

    sp = SamplingParams(temperature=0, max_tokens=48)
    kw = {"chat_template_kwargs": {"enable_thinking": False}}
    outs = llm.chat([chat(j[5]) for j in jobs], sp, **kw)
    rows = []
    for j, o in zip(jobs, outs):
        text = _norm(o.outputs[0].text)
        row = {"task": j[0], "arm": j[1], "src": j[2], "concept": j[3], "prompt": j[4], "out": text}
        if j[0] == "choose":
            low = text.lower()
            found = [c for c in concepts if c in low]
            row["pick"] = found[0] if len(found) == 1 else None
            row["ok"] = row["pick"] == j[3]
        else:
            row["ok"] = _lexicon_id(text, lex) == j[3]
        rows.append(row)

    def toks(s):
        return len(tok.encode(s)) if s else 0

    tokens = {}
    for it in payload["items"]:
        for arm, ctx in (("sentences", it["sentences"]), ("text", it["text"]), ("card", it["card"])):
            tokens.setdefault(arm, []).append(toks(ctx))
    summary = {}
    for task in ("choose", "continue"):
        for arm in ("sentences", "text", "card", "none", "null"):
            r = [x for x in rows if x["task"] == task and x["arm"] == arm]
            if r:
                summary[f"{task}/{arm}"] = {"n": len(r), "correct": sum(x["ok"] for x in r), "acc": round(sum(x["ok"] for x in r) / len(r), 4)}
    return {"model": model, "smoke": smoke, "chance": 1 / len(concepts), "summary": summary,
            "mean_context_tokens": {a: round(sum(v) / len(v), 1) for a, v in tokens.items()},
            "wire_bytes": payload["items"][0]["wire_bytes"], "rows": rows}


@app.local_entrypoint()
def main(cards: str, model: str = MODEL, smoke: bool = False):
    payload = json.loads(Path(cards).read_text())
    res = read.remote(payload, model, smoke)
    out = STEER / "results/e8" / ("e8_card_reader_smoke.json" if smoke else "e8_card_reader.json")
    out.write_text(json.dumps(res, indent=1))
    print(json.dumps({"summary": res["summary"], "tokens": res["mean_context_tokens"]}, indent=1))
    print("wrote", out)
