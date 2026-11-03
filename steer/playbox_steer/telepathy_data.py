"""Text for the cross-family channel. Fit topics train the map; test concepts are never used to fit it. The lexicon used
to decode a concept is written separately from the sentences that build the thought vector, and shares few words."""
import random

_T = [
    "The {a} {n} {v} near the {n2} this morning.",
    "People often say the {n} is {a}, but the {n2} {v} anyway.",
    "Everyone noticed how the {a} {n} {v} beside the {n2}.",
    "A story about a {n} and a {n2} that {v} all day.",
]

_FIT = {
    "garden": (["flower", "soil", "seed", "hedge", "rake", "tulip", "weed", "trellis"], ["green", "fragrant", "overgrown", "tidy"], ["bloomed", "grew", "wilted", "spread"]),
    "kitchen": (["pan", "knife", "oven", "spoon", "recipe", "dough", "onion", "stove"], ["hot", "greasy", "fresh", "sharp"], ["sizzled", "simmered", "burned", "rose"]),
    "city": (["bus", "tower", "street", "crosswalk", "taxi", "subway", "plaza", "billboard"], ["crowded", "noisy", "bright", "busy"], ["honked", "rushed", "gleamed", "stalled"]),
    "school": (["teacher", "desk", "notebook", "lesson", "bell", "chalk", "backpack", "hallway"], ["quiet", "strict", "bored", "eager"], ["rang", "waited", "echoed", "paused"]),
    "farm": (["barn", "tractor", "cow", "field", "fence", "hay", "rooster", "silo"], ["muddy", "old", "wide", "golden"], ["rattled", "crowed", "stood", "swayed"]),
    "weather": (["cloud", "wind", "storm", "fog", "rain", "breeze", "frost", "thunder"], ["gray", "cold", "heavy", "gentle"], ["drifted", "rolled", "settled", "passed"]),
    "office": (["desk", "report", "meeting", "printer", "email", "manager", "folder", "calendar"], ["late", "tedious", "urgent", "formal"], ["arrived", "stalled", "piled", "ended"]),
    "river": (["bank", "current", "pebble", "bridge", "willow", "canoe", "reed", "ford"], ["slow", "cold", "clear", "winding"], ["flowed", "curved", "glittered", "drifted"]),
    "market": (["stall", "apple", "coin", "basket", "vendor", "scale", "crate", "awning"], ["cheap", "ripe", "loud", "colorful"], ["sold", "spilled", "rolled", "shone"]),
    "library": (["shelf", "page", "catalog", "ladder", "desk", "journal", "map", "lamp"], ["dusty", "silent", "tall", "worn"], ["creaked", "waited", "glowed", "stood"]),
    "workshop": (["hammer", "bench", "nail", "saw", "plank", "vise", "drill", "lathe"], ["rough", "heavy", "precise", "loud"], ["rang", "turned", "split", "held"]),
    "travel": (["suitcase", "ticket", "train", "platform", "passport", "hotel", "gate", "map"], ["delayed", "crowded", "distant", "tired"], ["departed", "waited", "arrived", "rolled"]),
    "family": (["table", "grandmother", "cousin", "photo", "porch", "dinner", "uncle", "kitchen"], ["warm", "noisy", "old", "cheerful"], ["laughed", "gathered", "argued", "sat"]),
    "hospital": (["nurse", "ward", "bed", "chart", "corridor", "clipboard", "monitor", "lobby"], ["sterile", "calm", "busy", "bright"], ["beeped", "waited", "hummed", "passed"]),
    "festival": (["lantern", "drum", "parade", "costume", "banner", "crowd", "stage", "float"], ["loud", "festive", "bright", "dazzling"], ["marched", "danced", "swirled", "shone"]),
    "mountain": (["ridge", "trail", "boulder", "pine", "summit", "valley", "cabin", "stream"], ["steep", "rocky", "cold", "quiet"], ["rose", "echoed", "wound", "stood"]),
}


def _make(topic: str, nouns, adjs, verbs, k: int = 36) -> list[str]:
    r = random.Random(topic)
    out = []
    for _ in range(k):
        n, n2 = r.sample(nouns, 2)
        out.append(r.choice(_T).format(a=r.choice(adjs), n=n, n2=n2, v=r.choice(verbs)))
    return out


FIT_TOPICS = {t: _make(t, *v) for t, v in _FIT.items()}

NEUTRAL = [
    "The item was placed on the table.", "She said that it would happen soon.", "There were several things to consider.",
    "He walked into the room and sat down.", "It was a normal day like any other.", "They talked about it for a while.",
    "The report was finished on time.", "Something was said and then forgotten.", "We looked at the page for a minute.",
    "The door opened and someone came in.", "A number of people agreed with the plan.", "The result was what we expected.",
    "He took a moment before answering.", "It seemed fine at the time.", "The two of them left together.",
    "That was all there was to say.",
]

PROMPTS = [
    "Lately I keep thinking about", "Today I want to tell you about", "The thing on my mind is",
    "If I had to describe my morning, it was all about", "My favorite subject is", "Here is a short story about",
    "Let me explain something regarding", "One more thing:",
]

TEST_CONCEPTS = {
    "ocean": {
        "sentences": [
            "Waves rolled across the open sea under a pale sky.", "The sailor watched the tide pull the boat toward deep water.",
            "Salt spray stung his face as the ship crested another swell.", "Whales and dolphins moved through the cold blue depths.",
            "The reef was full of coral, fish and drifting seaweed.", "A lighthouse stood on the rocks above the crashing surf.",
            "They dived below the surface to see the shipwreck.", "Gulls circled over the harbor as the fleet came in.",
        ],
        "lexicon": ["ocean", "sea", "seas", "wave", "waves", "water", "tide", "tides", "beach", "shore", "coast", "marine", "fish", "boat", "boats", "ship", "ships", "sailing", "sail", "whale", "whales", "shark", "sharks", "island", "islands", "dive", "diving", "swimming", "swim", "harbor", "fishing", "fisherman", "surf", "underwater", "aquatic", "navy", "naval", "pirate", "pirates", "voyage"],
    },
    "space": {
        "sentences": [
            "The rocket lifted off toward orbit above the planet.", "Astronauts floated inside the station as Earth turned below.",
            "A telescope revealed distant galaxies glowing in the dark.", "The probe crossed the asteroid belt on its way to Jupiter.",
            "Stars and nebulae filled the endless black of the cosmos.", "Mission control counted down to the moon landing.",
            "Gravity bent the light around the black hole.", "The comet's tail stretched across the night sky.",
        ],
        "lexicon": ["space", "star", "stars", "planet", "planets", "galaxy", "galaxies", "universe", "cosmic", "cosmos", "moon", "mars", "sun", "solar", "orbit", "rocket", "rockets", "nasa", "astronaut", "astronauts", "alien", "aliens", "telescope", "satellite", "satellites", "asteroid", "comet", "spacecraft", "spaceship", "milky", "jupiter", "saturn", "lunar", "extraterrestrial", "celestial", "interstellar"],
    },
    "music": {
        "sentences": [
            "The guitarist tuned his strings before the concert began.", "A choir sang a slow melody in perfect harmony.",
            "The drummer kept a steady rhythm under the piano chords.", "She wrote a song with a catchy chorus and a quiet bridge.",
            "The orchestra played a symphony in the grand hall.", "He practiced scales on the violin every evening.",
            "The band released a new album of rock and jazz.", "Listeners hummed along to the tune on the radio.",
        ],
        "lexicon": ["music", "song", "songs", "band", "bands", "guitar", "piano", "drums", "drummer", "singer", "sing", "singing", "album", "albums", "concert", "concerts", "melody", "rhythm", "jazz", "rock", "lyrics", "musician", "musicians", "orchestra", "violin", "chord", "chords", "beat", "beats", "tune", "tunes", "playlist", "record", "records", "vocal", "vocals", "composer", "symphony", "dance"],
    },
    "sport": {
        "sentences": [
            "The striker scored a goal in the final minute of the match.", "Fans roared as the team won the championship.",
            "The coach drilled the players on defense before the game.", "She sprinted the last lap and broke the record.",
            "The pitcher threw a fastball past the batter.", "Both teams trained hard for the tournament season.",
            "The referee blew the whistle to end the first half.", "He dunked the ball and the arena erupted.",
        ],
        "lexicon": ["game", "games", "team", "teams", "player", "players", "football", "soccer", "basketball", "baseball", "tennis", "golf", "coach", "coaches", "season", "league", "win", "won", "winning", "score", "scored", "champion", "championship", "tournament", "athlete", "athletes", "sport", "sports", "stadium", "fans", "match", "olympic", "olympics", "race", "running", "training", "ball", "goal"],
    },
    "medicine": {
        "sentences": [
            "The doctor examined the patient and ordered a blood test.", "Antibiotics cleared the infection within a week.",
            "Surgeons prepared the operating room for the heart procedure.", "The vaccine trial showed strong protection against the virus.",
            "She was diagnosed with diabetes and started treatment.", "Nurses checked the dosage before giving the medication.",
            "Symptoms included fever, cough and chest pain.", "The clinic offered screening for cancer and other diseases.",
        ],
        "lexicon": ["doctor", "doctors", "patient", "patients", "disease", "diseases", "health", "medical", "medicine", "medicines", "hospital", "treatment", "treatments", "cancer", "drug", "drugs", "symptoms", "symptom", "surgery", "clinical", "clinic", "therapy", "diagnosis", "infection", "virus", "vaccine", "cure", "illness", "pain", "blood", "heart", "diabetes", "pills", "nurse", "nurses", "healthcare", "medication", "care"],
    },
}
