"""E8 follow-up on a GPU node (k sweep and join test): python tools/e8_sweep_node.py out/e8_sweep.json"""
import json
import os
import sys
from pathlib import Path

os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from e8_node import REPOS, fetch  # noqa: E402


def main() -> None:
    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    from playbox_steer import hub_run

    res = hub_run.sweep({n: fetch(r) for n, r in REPOS.items()})
    out.write_text(json.dumps(res, indent=1))
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
