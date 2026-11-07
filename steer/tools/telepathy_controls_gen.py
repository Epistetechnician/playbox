"""Harder no-topic controls for the telepathy protocol: brain-written content-free filler sets (gate should mute them) and
sets about subjects that are not on B's topic list (an out-of-list probe: the gate should not mute these)."""
import concurrent.futures as cf, json, re, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
import telepathy_agentfit_gen as G

FILLER = ["small talk between strangers", "placeholder text for a form", "generic office email filler", "vague statements that say nothing",
          "polite greetings and goodbyes", "describing an unremarkable afternoon", "a list of bland everyday observations",
          "meeting scheduling chatter", "generic product description filler", "empty reassurances"]
OFFLIST = ["knitting", "gardening", "cooking pasta", "weather", "bus commutes", "filing taxes"]


def sets(kind, items, prompt):
    def one(x):
        body = {"model": "qwen3.8-27b", "temperature": 0.9, "max_tokens": 300, "chat_template_kwargs": {"enable_thinking": False},
                "messages": [{"role": "user", "content": prompt.format(x)}]}
        for _ in range(3):
            import urllib.request
            req = urllib.request.Request(G.URL, json.dumps(body).encode(), {"Authorization": f"Bearer {G.KEY}", "Content-Type": "application/json"})
            t = json.load(urllib.request.urlopen(req, timeout=600))["choices"][0]["message"]["content"]
            s = [re.sub(r"^[\-\*\d\.\)\s]+", "", l).strip() for l in t.splitlines() if l.strip()]
            s = [y for y in s if 8 < len(y) < 300]
            if len(s) >= 6:
                return {"kind": kind, "about": x, "sentences": s[:6]}
    with cf.ThreadPoolExecutor(8) as ex:
        return [r for r in ex.map(one, items) if r]


if __name__ == "__main__":
    out = sets("filler", FILLER, "Write 6 short sentences, one per line, no numbering: {}. They must not be about any real subject or topic.") + \
          sets("offlist", OFFLIST, "Write 6 short, concrete sentences, one per line, no numbering, about: {}.")
    (Path(__file__).parent.parent / "playbox_steer/telepathy_controls.json").write_text(json.dumps(out, indent=1))
    print(len(out), [o["kind"] for o in out].count("filler"), [o["kind"] for o in out].count("offlist"))
