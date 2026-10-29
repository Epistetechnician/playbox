import random

import numpy as np

from playbox_world.embedders import HashEmbedder, LSAEmbedder
from playbox_world.events import Event
from playbox_world.layout import fit_layout
from playbox_world.scene import topic_colors
from playbox_world.usability import make_packet, purity, score

POOLS = {
    "build": "compile linker cargo rustc artifact target release profile dependency crate".split(),
    "review": "reviewer approve comment diff nitpick feedback suggestion patch merge blocker".split(),
    "deploy": "rollout canary staging cluster replica ingress helm manifest region traffic".split(),
}


def corpus(n=150, seed=1):
    """Events share few words even within a topic, so word overlap alone is a weak signal."""
    rng = random.Random(seed)
    evs, truth = [], []
    for i in range(n):
        topic = rng.choice(list(POOLS))
        evs.append(Event(i / n, rng.choice(["claude", "codex"]), "message", " ".join(rng.sample(POOLS[topic], 3))))
        truth.append(topic)
    return evs, truth


def cluster_purity(embedder):
    evs, truth = corpus()
    pos, _ = fit_layout(evs, ["claude", "codex"], steps=300, embedder=embedder)
    _, lab = topic_colors(evs, pos[:, :2], k=3)
    return purity(lab, truth), evs, lab


def test_lsa_recovers_topics_that_share_few_words():
    p_lsa, _, _ = cluster_purity(LSAEmbedder())
    p_hash, _, _ = cluster_purity(HashEmbedder())
    print(f"purity lsa={p_lsa:.2f} hash={p_hash:.2f}")
    assert p_lsa >= 0.9
    assert p_lsa >= p_hash


def test_packet_scores_raters_against_the_hidden_answer():
    _, evs, lab = cluster_purity(LSAEmbedder())
    packet = make_packet(evs, lab)
    assert len(packet) == 3 and all(len(p["options"]) == 3 for p in packet)
    perfect = [p["answer"] for p in packet]
    wrong = [(p["answer"] + 1) % 3 for p in packet]
    assert score(packet, [perfect] * 4)["passed"]
    assert score(packet, [perfect, perfect, perfect, wrong])["passed"]       # 3 of 4
    assert not score(packet, [perfect, perfect, wrong, wrong])["passed"]     # 2 of 4


def test_labels_skip_tool_ids_flags_and_paths():
    from playbox_world.usability import label_words
    got = list(label_words("mcp__ccd_turn__wrap_up Bash 2>&1 ~/Documents/x <<'EOF' parser fixed, tokenizer."))
    assert got == ["parser", "fixed", "tokenizer"]
