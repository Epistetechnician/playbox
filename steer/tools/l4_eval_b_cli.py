"""python tools/l4_eval_b_cli.py ckpt.pt out.json   (the settling test for (B): many draws, three draw seeds, topic-cluster bootstrap)"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import l4_eval_b as E  # noqa: E402

r = E.run(sys.argv[1], log=lambda *x, **k: print(*x, flush=True))
Path(sys.argv[2]).write_text(json.dumps(r, indent=1))
print(json.dumps({k: r[k] for k in ("retention_pooled", "B_mapped_minus_unrelated_real", "specificity_mapped_minus_mapped_wrong", "by_draw_seed")}))
