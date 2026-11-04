"""Generate agent-style fit text for the L2 map: the Qwen3.8 brain writes 6 short concrete sentences about each fit topic,
the same instruction Agent A gets in telepathy_hermes.py. Output: playbox_steer/telepathy_agentfit.json {topic: [sentences]}."""
import concurrent.futures as cf, json, os, re, sys, urllib.request
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent.parent))
from playbox_steer.telepathy_data_xl import FIT_TOPICS_XL

CONF = Path.home() / ".config/playbox"
URL = (CONF / "brain_url").read_text().strip() + "/v1/chat/completions"
KEY = (CONF / "brain_token").read_text().strip()
OUT = Path(__file__).parent.parent / "playbox_steer/telepathy_agentfit.json"
SAMPLES = int(os.environ.get("SAMPLES", 4))


def ask(topic: str, k: int) -> list[str]:
    body = {"model": "qwen3.8-27b", "temperature": 0.9, "max_tokens": 400,
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user", "content": f"Your secret topic is: {topic.replace('_', ' ')}. Write 6 short, concrete sentences "
                          f"about it, one per line, no numbering, no preamble. Vary the angle in each sentence."}]}
    for _ in range(3):
        try:
            req = urllib.request.Request(URL, json.dumps(body).encode(), {"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"})
            txt = json.load(urllib.request.urlopen(req, timeout=600))["choices"][0]["message"]["content"]
            s = [re.sub(r"^[\-\*\d\.\)\s]+", "", l).strip() for l in txt.splitlines() if l.strip()]
            s = [x for x in s if 15 < len(x) < 300]
            if len(s) >= 4:
                return s[:6]
        except Exception as e:
            print("retry", topic, e, flush=True)
    return []


if __name__ == "__main__":
    topics = list(FIT_TOPICS_XL)
    out = {}
    with cf.ThreadPoolExecutor(24) as ex:
        futs = {ex.submit(ask, t, k): t for t in topics for k in range(SAMPLES)}
        for f in cf.as_completed(futs):
            out.setdefault(futs[f], []).extend(f.result())
    out = {t: s for t, s in out.items() if s}
    OUT.write_text(json.dumps(out, indent=0))
    print(len(out), "topics", sum(map(len, out.values())), "sentences")
