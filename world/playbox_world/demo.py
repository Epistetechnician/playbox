"""python -m playbox_world.demo [events.jsonl] [out.png]"""

import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import torch

from .events import KIND_MASS, load_jsonl, synthetic
from .layout import fit_layout
from .scene import fit_field, render, topic_colors


def main():
    src = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1].endswith(".jsonl") else None
    out = sys.argv[-1] if sys.argv[-1].endswith(".png") else "world.png"
    events = load_jsonl(src) if src else synthetic()
    agents = sorted({e.agent for e in events})
    pos, layout_loss = fit_layout(events, agents)
    colors, labels = topic_colors(events, pos[:, :2])
    mass = torch.tensor([KIND_MASS[e.kind] for e in events])
    field, field_loss = fit_field(pos, mass, colors)
    print(f"events={len(events)} layout_loss={layout_loss:.4f} field_loss={field_loss:.5f}")
    fig, axes = plt.subplots(1, 4, figsize=(12, 3.2))
    for ax, az in zip(axes, (0.3, 1.5, 2.7, 4.2)):
        ax.imshow(render(field, az).clip(0, 1))
        ax.axis("off")
    fig.patch.set_facecolor("black")
    fig.savefig(out, dpi=120, facecolor="black", bbox_inches="tight")
    print("wrote", out)


if __name__ == "__main__":
    main()
