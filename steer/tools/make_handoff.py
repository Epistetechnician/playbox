"""Write a signed handoff envelope for tests: python make_handoff.py WEIGHTS_DIR WEIGHTS_HEX SEED_HEX TEXT OUT [CACHE_FROM_TEXT].

The cache is computed from CACHE_FROM_TEXT (default TEXT). A different CACHE_FROM_TEXT makes a cheater: a validly
signed record whose cache does not belong to the text it names.
"""
import sys
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from playbox_steer import engine, handoff, latent  # noqa: E402

weights_dir, weights_hex, seed, text, out = sys.argv[1:6]
source = sys.argv[6] if len(sys.argv) > 6 else text
model, tok = engine.load(Path(weights_dir))
key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(seed))
Path(out).write_bytes(handoff.pack(key, weights_hex, text, latent.read_context(model, tok, source, None)))
