import numpy as np
import torch

from playbox_world.events import KIND_MASS, synthetic
from playbox_world.hashgrid import Field, HashGrid
from playbox_world.layout import fit_layout
from playbox_world.scene import fit_field, render, topic_colors


def test_hashgrid_shape_and_gradient():
    g = HashGrid(levels=4, table_log2=10)
    x = torch.rand(32, 3)
    out = g(x)
    assert out.shape == (32, g.out_dim)
    out.sum().backward()
    assert g.tables.grad.abs().sum() > 0


def test_hashgrid_is_continuous():
    g = HashGrid(levels=4, table_log2=10)
    x = torch.rand(64, 3) * 0.9 + 0.05
    assert (g(x) - g(x + 1e-5)).abs().max() < 1e-3


def test_layout_separates_topics_and_follows_time():
    events = synthetic(120)
    agents = sorted({e.agent for e in events})
    pos, _ = fit_layout(events, agents, steps=300)
    _, lab = topic_colors(events, pos[:, :2])
    xy = pos[:, :2].numpy()
    same = np.mean([np.linalg.norm(xy[i] - xy[j]) for i in range(len(xy)) for j in range(i) if lab[i] == lab[j]])
    diff = np.mean([np.linalg.norm(xy[i] - xy[j]) for i in range(len(xy)) for j in range(i) if lab[i] != lab[j]])
    assert same < diff
    t = torch.tensor([e.t for e in events])
    assert torch.corrcoef(torch.stack([pos[:, 2], t]))[0, 1] > 0.99


def test_field_is_denser_at_events_than_far_away():
    events = synthetic(60)
    agents = sorted({e.agent for e in events})
    pos, _ = fit_layout(events, agents, steps=150)
    colors, _ = topic_colors(events, pos[:, :2])
    mass = torch.tensor([KIND_MASS[e.kind] for e in events])
    field, _ = fit_field(pos, mass, colors, steps=250)
    with torch.no_grad():
        at_events, _ = field(pos)
        far, _ = field(torch.rand(500, 3) * 0.0 + 0.99)
    assert at_events.mean() > 5 * far.mean()
    assert render(field, 0.3, size=16, samples=16).shape == (16, 16, 3)
