"""python tools/l4_eval_cli.py ckpt.pt out.json   (fp32 held-out evaluation of one trained mapper)"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import l4_eval as E  # noqa: E402

r = E.run(sys.argv[1], log=lambda *x: print(*x, flush=True))
Path(sys.argv[2]).write_text(json.dumps(r, indent=1))
print(json.dumps({k: r[k] for k in ("wikitext", "concept_retrieval_21way")})[:1200])
