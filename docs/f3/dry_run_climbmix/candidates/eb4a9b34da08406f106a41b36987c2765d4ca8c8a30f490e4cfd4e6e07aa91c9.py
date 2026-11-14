# proposal: weight_decay_0
# proposal: muon_ns_steps_2
# proposal: muon_ns_steps_3
"""Pinned baseline for the climbmix substrate: the autoresearch GPT (karpathy/autoresearch train.py, MIT) with Muon+AdamW,
ported to the F3 candidate contract:  build_model(vocab, block, hp) and train(ctx, model).

Changes from upstream train.py: no module-level script, no time budget (the harness owns the clock and the stop rule),
LR/weight-decay schedules run over PLAN steps instead of elapsed time, embeddings stay fp32, no torch.compile (a candidate
that adds it pays the compile time on the clock), and the hyperparameters that set scale come from the spec (`hp`).
"""
import math
from dataclasses import dataclass

import torch
import torch.nn as nn
import torch.nn.functional as F

EMBEDDING_LR = 0.8
UNEMBEDDING_LR = 0.004
MATRIX_LR = 0.075
SCALAR_LR = 0.5
WEIGHT_DECAY = 0.0
ADAM_BETAS = (0.8, 0.95)
WARMUP_RATIO = 0.0
WARMDOWN_RATIO = 0.5
FINAL_LR_FRAC = 0.0
GRAD_CLIP = 0.0
WINDOW_PATTERN = "L"


@dataclass
class GPTConfig:
    sequence_len: int = 2048
    vocab_size: int = 8192
    n_layer: int = 8
    n_head: int = 4
    n_kv_head: int = 4
    n_embd: int = 512
    window_pattern: str = "L"


def norm(x):
    return F.rms_norm(x, (x.size(-1),))


def has_ve(layer_idx, n_layer):
    return layer_idx % 2 == (n_layer - 1) % 2


def apply_rotary_emb(x, cos, sin):
    d = x.shape[3] // 2
    x1, x2 = x[..., :d], x[..., d:]
    cos, sin = cos.to(x.dtype), sin.to(x.dtype)
    return torch.cat([x1 * cos + x2 * sin, x1 * (-sin) + x2 * cos], 3)


class CausalSelfAttention(nn.Module):
    def __init__(self, config, layer_idx):
        super().__init__()
        self.n_head, self.n_kv_head, self.n_embd = config.n_head, config.n_kv_head, config.n_embd
        self.head_dim = self.n_embd // self.n_head
        assert self.n_embd % self.n_head == 0 and self.n_head % self.n_kv_head == 0
        self.c_q = nn.Linear(self.n_embd, self.n_head * self.head_dim, bias=False)
        self.c_k = nn.Linear(self.n_embd, self.n_kv_head * self.head_dim, bias=False)
        self.c_v = nn.Linear(self.n_embd, self.n_kv_head * self.head_dim, bias=False)
        self.c_proj = nn.Linear(self.n_embd, self.n_embd, bias=False)
        self.ve_gate_channels = 32
        self.ve_gate = nn.Linear(self.ve_gate_channels, self.n_kv_head, bias=False) if has_ve(layer_idx, config.n_layer) else None

    def forward(self, x, ve, cos_sin, window_size):
        B, T, C = x.size()
        q = self.c_q(x).view(B, T, self.n_head, self.head_dim)
        k = self.c_k(x).view(B, T, self.n_kv_head, self.head_dim)
        v = self.c_v(x).view(B, T, self.n_kv_head, self.head_dim)
        if ve is not None:
            ve = ve.view(B, T, self.n_kv_head, self.head_dim).to(v.dtype)
            gate = 2 * torch.sigmoid(self.ve_gate(x[..., : self.ve_gate_channels]))
            v = v + gate.unsqueeze(-1) * ve
        cos, sin = cos_sin
        q, k = apply_rotary_emb(q, cos, sin), apply_rotary_emb(k, cos, sin)
        q, k = norm(q), norm(k)
        k = k.repeat_interleave(self.n_head // self.n_kv_head, dim=2)
        v = v.repeat_interleave(self.n_head // self.n_kv_head, dim=2)
        q, k, v = q.transpose(1, 2), k.transpose(1, 2), v.transpose(1, 2)
        window = window_size[0]
        if 0 < window < T:
            mask = torch.ones(T, T, dtype=torch.bool, device=q.device).tril().triu(diagonal=1 - window)
            y = F.scaled_dot_product_attention(q, k, v, attn_mask=mask)
        else:
            y = F.scaled_dot_product_attention(q, k, v, is_causal=True)
        return self.c_proj(y.transpose(1, 2).contiguous().view(B, T, -1))


class MLP(nn.Module):
    def __init__(self, config):
        super().__init__()
        self.c_fc = nn.Linear(config.n_embd, 4 * config.n_embd, bias=False)
        self.c_proj = nn.Linear(4 * config.n_embd, config.n_embd, bias=False)

    def forward(self, x):
        return self.c_proj(F.relu(self.c_fc(x)).square())


class Block(nn.Module):
    def __init__(self, config, layer_idx):
        super().__init__()
        self.attn = CausalSelfAttention(config, layer_idx)
        self.mlp = MLP(config)

    def forward(self, x, ve, cos_sin, window_size):
        x = x + self.attn(norm(x), ve, cos_sin, window_size)
        return x + self.mlp(norm(x))


class GPT(nn.Module):
    def __init__(self, config):
        super().__init__()
        self.config = config
        self.window_sizes = self._window_sizes(config)
        self.transformer = nn.ModuleDict({"wte": nn.Embedding(config.vocab_size, config.n_embd),
                                          "h": nn.ModuleList([Block(config, i) for i in range(config.n_layer)])})
        self.lm_head = nn.Linear(config.n_embd, config.vocab_size, bias=False)
        self.resid_lambdas = nn.Parameter(torch.ones(config.n_layer))
        self.x0_lambdas = nn.Parameter(torch.zeros(config.n_layer))
        kv_dim = config.n_kv_head * (config.n_embd // config.n_head)
        self.value_embeds = nn.ModuleDict({str(i): nn.Embedding(config.vocab_size, kv_dim) for i in range(config.n_layer) if has_ve(i, config.n_layer)})
        self.rotary_seq_len = config.sequence_len * 2
        cos, sin = self._rotary(self.rotary_seq_len, config.n_embd // config.n_head)
        self.register_buffer("cos", cos, persistent=False)
        self.register_buffer("sin", sin, persistent=False)

    @torch.no_grad()
    def init_weights(self):
        nn.init.normal_(self.transformer.wte.weight, mean=0.0, std=1.0)
        nn.init.normal_(self.lm_head.weight, mean=0.0, std=0.001)
        s = 3**0.5 * self.config.n_embd**-0.5
        for b in self.transformer.h:
            for w in (b.attn.c_q, b.attn.c_k, b.attn.c_v, b.mlp.c_fc):
                nn.init.uniform_(w.weight, -s, s)
            nn.init.zeros_(b.attn.c_proj.weight)
            nn.init.zeros_(b.mlp.c_proj.weight)
            if b.attn.ve_gate is not None:
                nn.init.zeros_(b.attn.ve_gate.weight)
        self.resid_lambdas.fill_(1.0)
        self.x0_lambdas.fill_(0.1)
        for ve in self.value_embeds.values():
            nn.init.uniform_(ve.weight, -s, s)

    def _rotary(self, seq_len, head_dim, base=10000):
        inv = 1.0 / (base ** (torch.arange(0, head_dim, 2, dtype=torch.float32) / head_dim))
        f = torch.outer(torch.arange(seq_len, dtype=torch.float32), inv)
        return f.cos()[None, :, None, :], f.sin()[None, :, None, :]

    def _window_sizes(self, config):
        pat = config.window_pattern.upper()
        long_w = config.sequence_len
        sizes = [((long_w if pat[i % len(pat)] == "L" else long_w // 2), 0) for i in range(config.n_layer)]
        sizes[-1] = (long_w, 0)
        return sizes

    def setup_optimizer(self):
        dm = self.config.n_embd
        matrix = list(self.transformer.h.parameters())
        scale = (dm / 768) ** -0.5
        groups = [
            dict(kind="adamw", params=list(self.lm_head.parameters()), lr=UNEMBEDDING_LR * scale, betas=ADAM_BETAS, eps=1e-10, weight_decay=0.0),
            dict(kind="adamw", params=list(self.transformer.wte.parameters()), lr=EMBEDDING_LR * scale, betas=ADAM_BETAS, eps=1e-10, weight_decay=0.0),
            dict(kind="adamw", params=list(self.value_embeds.parameters()), lr=EMBEDDING_LR * scale, betas=ADAM_BETAS, eps=1e-10, weight_decay=0.0),
            dict(kind="adamw", params=[self.resid_lambdas], lr=SCALAR_LR * 0.01, betas=ADAM_BETAS, eps=1e-10, weight_decay=0.0),
            dict(kind="adamw", params=[self.x0_lambdas], lr=SCALAR_LR, betas=(0.96, 0.95), eps=1e-10, weight_decay=0.0),
        ]
        for shape in sorted({p.shape for p in matrix}):
            groups.append(dict(kind="muon", params=[p for p in matrix if p.shape == shape], lr=MATRIX_LR, momentum=0.95, ns_steps=2, beta2=0.95, weight_decay=WEIGHT_DECAY))
        opt = MuonAdamW(groups)
        for g in opt.param_groups:
            g["initial_lr"] = g["lr"]
        return opt

    def forward(self, idx, targets=None):
        B, T = idx.size()
        cos_sin = self.cos[:, :T], self.sin[:, :T]
        x = norm(self.transformer.wte(idx))
        x0 = x
        for i, block in enumerate(self.transformer.h):
            x = self.resid_lambdas[i] * x + self.x0_lambdas[i] * x0
            ve = self.value_embeds[str(i)](idx) if str(i) in self.value_embeds else None
            x = block(x, ve, cos_sin, self.window_sizes[i])
        logits = self.lm_head(norm(x)).float()
        logits = 15 * torch.tanh(logits / 15)
        if targets is not None:
            return F.cross_entropy(logits.reshape(-1, logits.size(-1)), targets.reshape(-1), ignore_index=-1)
        return logits


polar_express_coeffs = [
    (8.156554524902461, -22.48329292557795, 15.878769915207462),
    (4.042929935166739, -2.808917465908714, 0.5000178451051316),
    (3.8916678022926607, -2.772484153217685, 0.5060648178503393),
    (3.285753657755655, -2.3681294933425376, 0.46449024233003106),
    (2.3465413258596377, -1.7097828382687081, 0.42323551169305323),
]


def adamw_step(p, grad, exp_avg, exp_avg_sq, step, lr, b1, b2, eps, wd):
    p.mul_(1 - lr * wd)
    exp_avg.lerp_(grad, 1 - b1)
    exp_avg_sq.lerp_(grad.square(), 1 - b2)
    denom = (exp_avg_sq / (1 - b2**step)).sqrt() + eps
    p.add_(exp_avg / denom, alpha=-lr / (1 - b1**step))


def muon_step(grads, params, mom_buf, sec_buf, momentum, lr, wd, beta2, ns_steps, red_dim):
    mom_buf.lerp_(grads, 1 - momentum)
    g = grads.lerp_(mom_buf, momentum)
    X = g.bfloat16()
    X = X / (X.norm(dim=(-2, -1), keepdim=True) * 1.02 + 1e-6)
    if g.size(-2) > g.size(-1):
        for a, b, c in polar_express_coeffs[:ns_steps]:
            A = X.mT @ X
            X = a * X + X @ (b * A + c * (A @ A))
    else:
        for a, b, c in polar_express_coeffs[:ns_steps]:
            A = X @ X.mT
            X = a * X + (b * A + c * (A @ A)) @ X
    g = X
    v_mean = g.float().square().mean(dim=red_dim, keepdim=True)
    red_size = g.size(red_dim)
    v_norm = (v_mean.sum(dim=(-2, -1), keepdim=True) * red_size).sqrt()
    sec_buf.lerp_(v_mean.to(sec_buf.dtype), 1 - beta2)
    step_size = sec_buf.clamp_min(1e-10).rsqrt()
    v_norm_new = ((v_mean * red_size) * step_size.float().square()).sum(dim=(-2, -1), keepdim=True).sqrt()
    g = g * (step_size * (v_norm / v_norm_new.clamp_min(1e-10))).to(g.dtype)
    mask = (g * params) >= 0
    params.sub_(lr * g + lr * wd * params * mask)


class MuonAdamW(torch.optim.Optimizer):
    def __init__(self, groups):
        super().__init__(groups, defaults={})

    @torch.no_grad()
    def step(self):
        for group in self.param_groups:
            if group["kind"] == "adamw":
                for p in group["params"]:
                    if p.grad is None:
                        continue
                    st = self.state[p]
                    if not st:
                        st.update(step=0, exp_avg=torch.zeros_like(p), exp_avg_sq=torch.zeros_like(p))
                    st["step"] += 1
                    adamw_step(p, p.grad, st["exp_avg"], st["exp_avg_sq"], st["step"], group["lr"], group["betas"][0], group["betas"][1], group["eps"], group["weight_decay"])
            else:
                params = group["params"]
                if not params:
                    continue
                p0 = params[0]
                st = self.state[p0]
                n, shape = len(params), p0.shape
                if "momentum_buffer" not in st:
                    st["momentum_buffer"] = torch.zeros(n, *shape, dtype=p0.dtype, device=p0.device)
                    st["second_momentum_buffer"] = torch.zeros((n, shape[-2], 1) if shape[-2] >= shape[-1] else (n, 1, shape[-1]), dtype=p0.dtype, device=p0.device)
                red_dim = -1 if shape[-2] >= shape[-1] else -2
                sg, sp = torch.stack([p.grad for p in params]), torch.stack(params)
                muon_step(sg, sp, st["momentum_buffer"], st["second_momentum_buffer"], group["momentum"], group["lr"] * max(1.0, shape[-2] / shape[-1]) ** 0.5,
                          group["weight_decay"], group["beta2"], group["ns_steps"], red_dim)
                torch._foreach_copy_(params, list(sp.unbind(0)))


def build_model(vocab_size, block_size, hp):
    dim = ((hp["depth"] * hp["aspect_ratio"] + hp["head_dim"] - 1) // hp["head_dim"]) * hp["head_dim"]
    heads = dim // hp["head_dim"]
    model = GPT(GPTConfig(sequence_len=block_size, vocab_size=vocab_size, n_layer=hp["depth"], n_head=heads, n_kv_head=heads, n_embd=dim, window_pattern=WINDOW_PATTERN))
    model.init_weights()
    return model


def train(ctx, model):
    hp = ctx.hparams
    model.to(ctx.device)
    opt = model.setup_optimizer()
    per = hp["device_batch"] * ctx.block_size
    accum = hp["total_batch"] // per
    assert hp["total_batch"] % per == 0
    step = 0
    while True:
        for _ in range(accum):
            x, y = ctx.next_batch(hp["device_batch"])
            with ctx.autocast():
                loss = model(x, y)
            (loss / accum).backward()
        progress = min(step / hp["plan_steps"], 1.0)
        if progress < WARMUP_RATIO:
            lrm = progress / WARMUP_RATIO
        elif progress < 1.0 - WARMDOWN_RATIO:
            lrm = 1.0
        else:
            cool = (1.0 - progress) / WARMDOWN_RATIO
            lrm = cool + (1 - cool) * FINAL_LR_FRAC
        mom = (1 - min(step / 300, 1)) * 0.85 + min(step / 300, 1) * 0.95
        for g in opt.param_groups:
            g["lr"] = g["initial_lr"] * lrm
            if g["kind"] == "muon":
                g["momentum"] = mom
                g["weight_decay"] = WEIGHT_DECAY * (1 - progress)
        if GRAD_CLIP > 0:
            torch.nn.utils.clip_grad_norm_(model.parameters(), GRAD_CLIP)
        opt.step()
        model.zero_grad(set_to_none=True)
        if not math.isfinite(loss.item()):
            raise RuntimeError("non-finite loss")
        step += 1
        if step % ctx.eval_every == 0:
            ctx.eval_dev(model)  # the harness raises here when the target is reached or a cap is hit
