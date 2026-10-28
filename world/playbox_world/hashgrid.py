"""Multiresolution hash encoding (Müller et al. 2022), pure PyTorch."""

import math

import torch
from torch import nn

_PRIMES = (1, 2654435761, 805459861)
_CORNERS = [(dx, dy, dz) for dx in (0, 1) for dy in (0, 1) for dz in (0, 1)]


class HashGrid(nn.Module):
    def __init__(self, levels=8, table_log2=14, features=2, base_res=8, max_res=128):
        super().__init__()
        growth = math.exp((math.log(max_res) - math.log(base_res)) / max(levels - 1, 1))
        self.resolutions = [int(base_res * growth**level) for level in range(levels)]
        self.table_size = 2**table_log2
        self.out_dim = levels * features
        self.tables = nn.Parameter(torch.empty(levels, self.table_size, features).uniform_(-1e-2, 1e-2))

    def _hash(self, idx: torch.Tensor) -> torch.Tensor:
        h = idx[:, 0] * _PRIMES[0]
        h = h ^ (idx[:, 1] * _PRIMES[1])
        h = h ^ (idx[:, 2] * _PRIMES[2])
        return h & (self.table_size - 1)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        """x: (N, 3) in [0, 1]. Returns (N, levels * features)."""
        x = x.clamp(0.0, 1.0 - 1e-6)
        outs = []
        for level, res in enumerate(self.resolutions):
            p = x * res
            p0 = p.floor()
            frac = p - p0
            p0 = p0.long()
            acc = 0.0
            for dx, dy, dz in _CORNERS:
                corner = torch.tensor([dx, dy, dz], device=x.device)
                w = torch.where(corner.bool(), frac, 1.0 - frac).prod(dim=1, keepdim=True)
                acc = acc + w * self.tables[level][self._hash(p0 + corner)]
            outs.append(acc)
        return torch.cat(outs, dim=1)


class Field(nn.Module):
    """Position -> (density, rgb)."""

    def __init__(self, **grid_kwargs):
        super().__init__()
        self.grid = HashGrid(**grid_kwargs)
        self.mlp = nn.Sequential(nn.Linear(self.grid.out_dim, 64), nn.ReLU(), nn.Linear(64, 64), nn.ReLU(), nn.Linear(64, 4))

    def forward(self, x: torch.Tensor):
        raw = self.mlp(self.grid(x))
        return nn.functional.softplus(raw[:, 0] - 1.0), torch.sigmoid(raw[:, 1:])
