# proposal: layers_1
# proposal: lr_6e-3
# proposal: lr_3e-3
"""Pinned baseline candidate: a tiny GPT trained with constant-LR AdamW. Scripted stand-ins patch this file."""
import torch
import torch.nn as nn
import torch.nn.functional as F

LR = 6e-3
WD = 0.0
BATCH = 32
N_LAYER = 1
D_MODEL = 64
N_HEAD = 4
WARMUP = 0
CLIP = 0.0


class Block(nn.Module):
    def __init__(self, d, h):
        super().__init__()
        self.h = h
        self.ln1 = nn.LayerNorm(d)
        self.qkv = nn.Linear(d, 3 * d)
        self.proj = nn.Linear(d, d)
        self.ln2 = nn.LayerNorm(d)
        self.fc = nn.Linear(d, 4 * d)
        self.out = nn.Linear(4 * d, d)

    def forward(self, x):
        B, T, C = x.shape
        q, k, v = self.qkv(self.ln1(x)).split(C, dim=2)
        q, k, v = (t.view(B, T, self.h, C // self.h).transpose(1, 2) for t in (q, k, v))
        a = F.scaled_dot_product_attention(q, k, v, is_causal=True).transpose(1, 2).reshape(B, T, C)
        x = x + self.proj(a)
        return x + self.out(F.gelu(self.fc(self.ln2(x))))


class GPT(nn.Module):
    def __init__(self, vocab, block, d, layers, heads):
        super().__init__()
        self.tok = nn.Embedding(vocab, d)
        self.pos = nn.Embedding(block, d)
        self.blocks = nn.ModuleList(Block(d, heads) for _ in range(layers))
        self.lnf = nn.LayerNorm(d)
        self.head = nn.Linear(d, vocab, bias=False)

    def forward(self, x):
        h = self.tok(x) + self.pos(torch.arange(x.shape[1]))
        for b in self.blocks:
            h = b(h)
        return self.head(self.lnf(h))


def build_model(vocab_size, block_size):
    return GPT(vocab_size, block_size, D_MODEL, N_LAYER, N_HEAD)


def train(ctx, model):
    opt = torch.optim.AdamW(model.parameters(), lr=LR, weight_decay=WD)
    step = 0
    while True:
        x, y = ctx.next_batch(BATCH)
        loss = F.cross_entropy(model(x).reshape(-1, ctx.vocab_size), y.reshape(-1))
        opt.zero_grad()
        loss.backward()
        if CLIP:
            torch.nn.utils.clip_grad_norm_(model.parameters(), CLIP)
        if WARMUP:
            for g in opt.param_groups:
                g["lr"] = LR * min(1.0, (step + 1) / WARMUP)
        opt.step()
        step += 1
        if step % ctx.eval_every == 0:
            ctx.eval_dev(model)  # the harness raises here when the target is reached or a cap is hit
