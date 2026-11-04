"""More anchor text for the hub (docs/latent-channels.md 6d): 16 further topics built exactly like `FIT_TOPICS`.
They are only ever added to the training anchors. The held-out sentences and the five test concepts do not change, so
results stay comparable with the 426-anchor runs. No word here is in a test concept's lexicon (checked by a test)."""
from .telepathy_data import _make

_EXTRA = {
    "bakery": (["loaf", "oven", "flour", "croissant", "counter", "pastry", "apron", "tray"], ["warm", "crusty", "sweet", "early"], ["baked", "cooled", "rose", "crackled"]),
    "forest": (["oak", "moss", "fern", "trunk", "squirrel", "canopy", "path", "mushroom"], ["dense", "damp", "ancient", "shady"], ["rustled", "creaked", "swayed", "grew"]),
    "desert": (["dune", "cactus", "lizard", "mirage", "canyon", "oasis", "scorpion", "wind"], ["dry", "scorching", "empty", "vast"], ["shimmered", "shifted", "baked", "stretched"]),
    "airport": (["runway", "terminal", "suitcase", "pilot", "gate", "luggage", "tower", "hangar"], ["busy", "delayed", "bright", "sleepy"], ["landed", "boarded", "roared", "waited"]),
    "museum": (["statue", "painting", "gallery", "exhibit", "curator", "vase", "tapestry", "guide"], ["ancient", "quiet", "polished", "fragile"], ["gleamed", "hung", "waited", "echoed"]),
    "bank": (["vault", "teller", "ledger", "loan", "account", "counter", "safe", "clerk"], ["formal", "secure", "tedious", "careful"], ["opened", "waited", "closed", "tallied"]),
    "courtroom": (["judge", "jury", "lawyer", "witness", "verdict", "bench", "gavel", "docket"], ["solemn", "tense", "formal", "calm"], ["rose", "listened", "paused", "ruled"]),
    "factory": (["conveyor", "press", "boiler", "assembly", "foreman", "pipe", "gear", "shift"], ["loud", "greasy", "tireless", "huge"], ["clanked", "hummed", "turned", "pounded"]),
    "castle": (["tower", "moat", "drawbridge", "banner", "knight", "dungeon", "throne", "wall"], ["grim", "ancient", "towering", "cold"], ["loomed", "stood", "echoed", "crumbled"]),
    "circus": (["tent", "clown", "juggler", "acrobat", "trapeze", "ringmaster", "elephant", "popcorn"], ["colorful", "giddy", "dizzy", "noisy"], ["tumbled", "gasped", "swung", "cheered"]),
    "zoo": (["giraffe", "cage", "keeper", "penguin", "tiger", "enclosure", "monkey", "turnstile"], ["curious", "sleepy", "hungry", "restless"], ["paced", "yawned", "roared", "watched"]),
    "garage": (["wrench", "jack", "tire", "engine", "toolbox", "hood", "bumper", "pit"], ["greasy", "cluttered", "cold", "rusty"], ["rattled", "idled", "clanged", "dripped"]),
    "theater": (["curtain", "balcony", "aisle", "usher", "script", "spotlight", "wing", "ticket"], ["dim", "hushed", "grand", "velvet"], ["rose", "fell", "glowed", "waited"]),
    "village": (["cottage", "well", "chimney", "lane", "church", "baker", "gate", "hedge"], ["quaint", "sleepy", "narrow", "friendly"], ["slept", "woke", "smoked", "stood"]),
    "playground": (["swing", "slide", "sandbox", "seesaw", "bench", "fence", "puddle", "rope"], ["noisy", "muddy", "bright", "crowded"], ["creaked", "squeaked", "swung", "shook"]),
    "laundromat": (["dryer", "washer", "basket", "sock", "detergent", "coin", "bench", "sheet"], ["warm", "humming", "damp", "empty"], ["spun", "tumbled", "buzzed", "folded"]),
}

EXTRA_TOPICS = {t: _make(t, *v) for t, v in _EXTRA.items()}


def extra_anchors(topics: int, exclude: set[str]) -> list[str]:
    """Sentences of the first `topics` extra topics, in a fixed order, without duplicates or anything in `exclude`."""
    out, seen = [], set(exclude)
    for sents in list(EXTRA_TOPICS.values())[:topics]:
        for s in sents:
            if s not in seen:
                seen.add(s)
                out.append(s)
    return out
