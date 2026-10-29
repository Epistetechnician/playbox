"""Learn where each event lives in the unit cube.

z follows time. x, y are learned so that semantically similar events sit close and
dissimilar ones are pushed apart. This is the part with no ground truth: the scene is
whatever this objective makes it.
"""

import numpy as np
import torch
from torch import nn

from .embedders import get_embedder
from .events import KIND_MASS, KINDS, Event

_AGENT_COLORS = None


def event_tensors(events: list[Event], agents: list[str], embedder=None):
    texts = [e.text for e in events]
    emb = torch.tensor(np.asarray((embedder or get_embedder()).fit(texts)(texts), dtype=np.float32))
    t = torch.tensor([e.t for e in events], dtype=torch.float32)
    agent = torch.tensor([agents.index(e.agent) for e in events])
    kind = torch.tensor([KINDS.index(e.kind) for e in events])
    mass = torch.tensor([KIND_MASS[e.kind] for e in events], dtype=torch.float32)
    return emb, t, agent, kind, mass


class Layout(nn.Module):
    def __init__(self, n_agents: int, dim: int):
        super().__init__()
        self.agent_emb = nn.Embedding(n_agents, 8)
        self.net = nn.Sequential(nn.Linear(dim + 8, 64), nn.ReLU(), nn.Linear(64, 2))

    def forward(self, emb, t, agent):
        xy = torch.sigmoid(self.net(torch.cat([emb, self.agent_emb(agent)], dim=1)))
        z = (0.1 + 0.8 * t)[:, None]
        return torch.cat([0.1 + 0.8 * xy, z], dim=1)


def fit_layout(events, agents, steps=400, margin=0.35, seed=0, embedder=None):
    torch.manual_seed(seed)
    emb, t, agent, _, _ = event_tensors(events, agents, embedder)
    model = Layout(len(agents), emb.shape[1])
    opt = torch.optim.Adam(model.parameters(), lr=1e-2)
    sim = (emb @ emb.T).clamp(min=0)
    sim = (sim / sim.max()).detach()
    for _ in range(steps):
        pos = model(emb, t, agent)
        d2 = ((pos[:, None, :2] - pos[None, :, :2]) ** 2).sum(-1)
        d = (d2 + 1e-8).sqrt()
        loss = (sim * d2 + (1 - sim) * torch.relu(margin - d) ** 2).mean()
        opt.zero_grad()
        loss.backward()
        opt.step()
    with torch.no_grad():
        return model(emb, t, agent), float(loss)
