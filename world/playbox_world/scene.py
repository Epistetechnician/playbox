"""Fit a hash-grid field to the laid-out events, then volume-render it."""

import math

import numpy as np
import torch

from .hashgrid import Field

TOPIC_PALETTE = torch.tensor([[0.95, 0.35, 0.30], [0.25, 0.60, 0.95], [0.35, 0.85, 0.45], [0.95, 0.80, 0.25]])


def _targets(x, pos, mass, colors, sigma):
    """Density and colour of the event cloud at query points x."""
    d2 = ((x[:, None, :] - pos[None, :, :]) ** 2).sum(-1)
    w = torch.exp(-d2 / (2 * sigma**2)) * mass[None, :]
    dens = w.sum(1)
    col = (w[:, :, None] * colors[None]).sum(1) / (dens[:, None] + 1e-6)
    return 1 - torch.exp(-dens), col  # occupancy in [0, 1)


def fit_field(pos, mass, colors, steps=600, sigma=0.03, batch=4096, seed=0, device="cpu"):
    torch.manual_seed(seed)
    pos, mass, colors = pos.to(device), mass.to(device), colors.to(device)
    field = Field().to(device)
    opt = torch.optim.Adam(field.parameters(), lr=1e-2)
    last = 0.0
    for _ in range(steps):
        near = pos[torch.randint(len(pos), (batch // 2,), device=device)] + sigma * 2 * torch.randn(batch // 2, 3, device=device)
        x = torch.cat([near, torch.rand(batch // 2, 3, device=device)]).clamp(0, 1)
        occ, col = _targets(x, pos, mass, colors, sigma)
        sigma_pred, rgb = field(x)
        occ_pred = 1 - torch.exp(-sigma_pred * 0.05)
        loss = ((occ_pred - occ) ** 2).mean() + (occ[:, None] * (rgb - col) ** 2).mean()
        opt.zero_grad()
        loss.backward()
        opt.step()
        last = float(loss)
    return field, last


@torch.no_grad()
def render(field, azimuth, elevation=0.35, size=96, samples=64, radius=1.6, device="cpu"):
    """Orbit camera looking at the cube centre; emission-absorption volume rendering."""
    c = torch.tensor([0.5, 0.5, 0.5], device=device)
    cam = c + radius * torch.tensor(
        [math.cos(elevation) * math.cos(azimuth), math.cos(elevation) * math.sin(azimuth), math.sin(elevation)], device=device
    )
    fwd = (c - cam) / (c - cam).norm()
    right = torch.cross(fwd, torch.tensor([0.0, 0.0, 1.0], device=device), dim=0)
    right = right / right.norm()
    up = torch.cross(right, fwd, dim=0)
    px = (torch.arange(size, device=device) + 0.5) / size * 2 - 1
    gy, gx = torch.meshgrid(-px, px, indexing="ij")
    dirs = fwd[None, None] + 0.5 * (gx[..., None] * right + gy[..., None] * up)
    dirs = (dirs / dirs.norm(dim=-1, keepdim=True)).reshape(-1, 3)
    ts = torch.linspace(radius - 0.9, radius + 0.9, samples, device=device)
    dt = float(ts[1] - ts[0])
    pts = cam[None, None] + dirs[:, None] * ts[None, :, None]  # (R, S, 3)
    inside = ((pts >= 0) & (pts <= 1)).all(-1)
    sig, rgb = field(pts.reshape(-1, 3))
    sig = sig.reshape(-1, samples) * inside
    rgb = rgb.reshape(-1, samples, 3)
    alpha = 1 - torch.exp(-sig * dt * 20)
    trans = torch.cumprod(torch.cat([torch.ones_like(alpha[:, :1]), 1 - alpha[:, :-1] + 1e-10], dim=1), dim=1)
    img = ((alpha * trans)[..., None] * rgb).sum(1)
    return img.reshape(size, size, 3).cpu().numpy()


def topic_colors(events, positions_xy, k=3):
    """Colour each event by the nearest of k xy cluster centres found by a few k-means steps."""
    p = positions_xy.numpy()
    rng = np.random.default_rng(0)
    centres = p[rng.choice(len(p), k, replace=False)]
    for _ in range(20):
        lab = np.argmin(((p[:, None] - centres[None]) ** 2).sum(-1), axis=1)
        for j in range(k):
            if (lab == j).any():
                centres[j] = p[lab == j].mean(0)
    return TOPIC_PALETTE[torch.tensor(lab)], lab
