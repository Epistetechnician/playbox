# L3 swarm demo: same-model agents hand each other context state through LMCache

Tier L3 of `docs/latent-channels.md`: agents running the same model share the model's KV cache instead of re-sending and
re-reading text. This page records a working demo on Modal, what it measured, and what it does not show.

Code: `steer/playbox_steer/l3_swarm.py` (experiment, stdlib only), `steer/tools/l3_lmcache_modal.py` (Modal app).
Raw results: `steer/results/l3_lmcache/l3.json` (full run), `l3_smoke.json` (2-trial check).
Cost: three runs of about 6 to 8 minutes on one L40S, roughly 1 USD in all. The app is a one-shot function, so nothing stays deployed.

## Result in one paragraph

Four vLLM engines (one writer A, three readers B1 to B3) serve Qwen3-1.7B-Base against one LMCache multiprocess server. A
prefills a 2.4k-token private case file once. Readers then answer questions about it while computing only 7% of the prompt
themselves (173 of 2,450 tokens); the other 2,315 tokens (94%) arrive as A's KV. Answers are exactly as accurate as when a
reader prefills everything itself (24/24 on each fact question; 70.8% on naming the topic, identical hit and cold), and 0% when the
case file is withheld. The price is bytes: 281 MB of KV against 10 KB of text, about 27,000 times more. Latency only improves
once the context is long (2.5x faster time to first token at 23k tokens; no gain at 2.4k).

LMCache is a **compute-sharing** mechanism, not a blind channel. See "What this does not show".

## Design

```
            private case file D (text)                       salt = capability, handle = (salt, token list)
                    |                                                  |
  agent A --> engine A: prefill D  --KV chunks-->  LMCache server  <--KV chunks--  engine B1..B3 <-- broker <-- agent B
                (computes 2,450 tokens)          (CPU memory, L1)               (computes ~173 tokens)   (holds the tokens)
```

- **Engines.** vLLM 0.25.1 with `LMCacheMPConnector`, `kv_role=kv_both`, prefix caching off, one shared LMCache 0.5.1 server
  (`lmcache server --chunk-size 256 --l1-size-gb 48`). All engines use the same model, block size 16, dtype and served name; this is
  LMCache's cache-identity contract (model, tensor-parallel size, KV dtype, page size, chunk size must all match).
- **Handle.** A reader receives a salt (`cache_salt`, mixed into the cache identity, so it works as a capability) and a question.
  A broker holds the token list behind the handle. The reading agent never holds the case file; the engine loads A's KV for it.
- **Fan-out.** One write, three readers, each asking a different question (topic, vault code, pilot callsign).
- **Chain ("blackboard in KV").** A prefills the case, B appends private notes (loading A's chunks, writing new ones), C asks about a fact in
  B's notes and loads both A's and B's chunks.
- **Case files.** 2.2k to 2.6k tokens: one topic from the 156-topic fit pool (the topic text repeated in shuffled order), with six unguessable facts
  (6-digit code, callsign, street, name, passphrase, account number) hidden at random lines. Questions need a specific fact or the topic out of 20 options.

## Measurements (24 trials, 72 questions, temperature 0)

| | Hit (salt held) | Cold (wrong salt, full prefill) | No context |
|---|---|---|---|
| vault code, exact | 24/24 | 24/24 | 0/24 |
| pilot callsign, exact | 24/24 | 24/24 | 0/24 |
| topic among 20 (chance 5%) | 17/24 (70.8%) | 17/24 (70.8%) | 0/24 |
| prompt tokens computed by the reader | 173 | 2,487 | n/a |
| prompt tokens loaded from LMCache | 2,315 | 0 (0 of 72 cold requests hit) | n/a |

- **Lossless.** Hit and cold give the same accuracy. The topic question is limited by the 1.7B base model, not by the cache.
- **Only complete 256-token chunks are shared**, so the tail of the prompt (here 100 to 250 tokens) is always recomputed.
- **Saved compute per case:** three reader questions loaded 6,944 tokens in total; A paid the 2,450-token prefill once.
- **Edit control (content addressing).** One fact edited mid-file, same salt: reuse stops at the edit (mean 928 tokens loaded, against 2,240 for the
  unedited file; edits in the first chunk loaded 0) and the answer follows the new text in 8/8 cases, the stale value in 0/8. The cache cannot leak
  or force an old value past a changed token; a chunk chain is verified by content.
- **Wrong handle.** Handed another case's handle with this case's question, the reader answered from the cached case in 95.7% of 69 trials and from
  this question's truth in 1.4%. Read this carefully: the request carries that case's tokens, so this shows addressing by content, not that
  the KV alone holds the answer.
- **Chain.** C loaded 2,784 tokens (A's and B's chunks) and computed 82. Accuracy 7/8 on a fact only B's notes contain, identical to the cold run (the
  one miss is shared, so it is the model, not the cache).
- **Bytes.** KV is 114,688 bytes per token for this model (28 layers x 8 KV heads x 128 x K and V x 2 bytes): 281 MB per case against 10.4 KB of text.

### Time to first token against context length (median of 5 requests, same question)

| tokens | KV size | text size | hit | cold | speedup |
|---|---|---|---|---|---|
| 2.5k | 284 MB | 10 KB | 112 ms | 96 ms | 0.9x |
| 6.1k | 704 MB | 26 KB | 126 ms | 178 ms | 1.4x |
| 11.8k | 1.35 GB | 52 KB | 209 ms | 350 ms | 1.7x |
| 23.3k | 2.68 GB | 104 KB | 338 ms | 839 ms | 2.5x |

Loading KV from host memory costs about as much as recomputing a short context on a fast GPU; the crossover here is near 3 to 5k tokens
for a 1.7B model. A larger model raises prefill cost per token, so the gain would start earlier and grow. Timings vary run to run: an
earlier run of the same code gave 193 ms against 806 ms at 23k tokens (4.2x). Treat the table as one sample on one L40S, not a benchmark.

## What this does not show

1. **It is not a text-free channel.** LMCache keys chunks by token content. To hit A's cache a reader must present A's exact token ids, so the text
   exists on the reader's engine host and is revealed to whoever builds the request. The demo hides the text from the reading *agent* through a
   broker, which is a trust assumption, not cryptography. A genuinely handle-addressed fetch (retrieve KV by opaque key, no tokens) is not available
   through vLLM's API and would have to be added in LMCache.
2. **Accuracy equality proves lossless reuse, not information transfer through the cache.** Hit and cold match because the tokens are in both
   requests. The edit control shows the cache cannot override the tokens; it does not show the model could answer from KV alone.
3. **Same model only.** Different weights, tokenizer, KV dtype, page size or chunk size mean no hit (or a wrong one; see security).
4. **One host.** Everything ran on one GPU and one CPU-memory pool. We did not test the remote tier, peer-to-peer sharing or the controller.
5. **Small scale.** 24 case files, one model size, synthetic text. The 1.7B base model also limits the topic question.

## Security notes

- **The salt is a capability, and it leaks.** LMCache's metrics are labeled `cache_salt="..."`, so anyone who can scrape the admin port
  (`:7000/metrics` in the demo) reads every live capability. Keep the admin port private, or use salts that are not secrets and add real auth.
- **Isolation held.** In 72 questions, a request with a wrong salt loaded 0 tokens from the cache.
- **Cache poisoning is plausible and untested.** Any engine with `kv_role=kv_both` can write. Two engines that agree on model name and layout but
  run different weights (a fine-tune, a LoRA merge) would write KV that readers trust. The content check covers tokens, not who produced the
  tensors. Use `kv_producer` and `kv_consumer` roles for readers that must not write, and restrict who can reach the daemon port.
- **A reader can leak.** The reader's engine host sees the full token list, and the generated answer can repeat any of it.
- **Memory.** 2.7 GB of KV per 23k-token case; a swarm can exhaust the pool. The demo used a 48 GB pool and LRU eviction.

## How it composes with the other tiers

| Tier | Same model | Different families | What moves |
|---|---|---|---|
| L2 vector (E7, 73.8% over 20 topics) | yes | yes | one concept vector, a few KB, lossy, text-free |
| L3 KV (this page) | yes, exact | no | full context state, hundreds of MB, lossless, compute saved, not text-free |
| L4 trained cache mapper | no | yes (planned) | translated KV, large, lossy, needs a mapper per ordered pair |

A router fits naturally: L3 when sender and receiver run the same model and share a long context (the common swarm case, where it saves
prefill and keeps one source of truth), L2 when the receivers are other families and only a concept needs to cross. L3 also gives the L4
work a fixed point: a mapper from a model to itself should recover L3's accuracy, which makes L3 the identity-case baseline for L4.

## Reproduce

```bash
modal run steer/tools/l3_lmcache_modal.py --smoke     # 2 trials
modal run steer/tools/l3_lmcache_modal.py             # 24 trials + 8 chain + scale sweep + edit control
```

## What I would build next

1. **Handle-addressed retrieval.** Add a key-based fetch to the LMCache server so a reader can load A's chunks by opaque handle without ever holding
   tokens. That would make the "reader never sees the text" property real instead of brokered.
2. **Position-independent reuse.** Test CacheBlend and KVCOMM-style anchor reuse so a reader can use A's KV when its own prompt differs from A's
   prefix (agents rarely share an exact prefix), and measure the accuracy cost.
3. **Where the gain is.** Repeat the sweep with a 7B to 8B model, 32k contexts and a remote or peer-to-peer tier, which is where latency and
   cross-host bandwidth decide whether KV beats text.
4. **Hybrid router with L2.** Same-model peers use L3, other families receive an L2 vector, and the router records which path moved what.
5. **Signed handles.** Bind the handle to a signature (the signed room envelope from the hub work) so a salt alone cannot be replayed.
