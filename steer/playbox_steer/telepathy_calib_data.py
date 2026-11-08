"""Negatives for calibrating the sender's gate on what the live Agent A actually writes. Fit and eval sets are disjoint."""
TOPICS = ["aviation", "baseball", "basketball", "cars", "chess", "classical-music", "cryptocurrency", "dinosaurs", "earthquake", "jazz",
          "law", "medicine", "ocean", "photography", "poker", "politics", "religion", "soccer", "space", "volcano"]

# "filler": A is told there is no secret topic and writes ordinary, forgettable sentences. One scene each.
FILLER_FIT = ["waiting for a kettle to boil", "picking a pen from a drawer", "walking down a hallway", "sitting in a quiet room", "turning off a light",
              "opening a window", "carrying a bag upstairs", "looking at a blank wall", "folding a towel", "closing a door",
              "reading a label on a jar", "putting a cup on a shelf", "standing at a bus stop", "waiting for a page to load", "tying a shoelace",
              "finding a key in a pocket", "watching rain on a window", "sweeping a floor", "stirring a pot", "setting a chair straight",
              "checking the time", "carrying a box across a room", "sitting on a bench", "wiping a table"]
FILLER_EVAL = ["unlocking a front door", "waiting in a queue", "pouring a glass of water", "hanging up a coat", "looking out of a window",
               "plugging in a charger", "sharpening a pencil", "watering a plant", "folding a letter", "scrolling past a notice"]

# "offlist": real subjects, but none of the 20 topics; the answer to these must be 'none'.
OFFLIST_FIT = ["weather", "bus commutes", "filing taxes", "gardening", "cooking pasta", "knitting", "plumbing", "traffic jams",
               "birthday parties", "house cleaning", "grocery shopping", "online shopping", "coffee", "tea", "hiking", "furniture"]
OFFLIST_EVAL = ["cats", "wedding planning", "painting a fence", "fishing", "bread baking", "moving house", "dentist visits", "train delays"]

NEUTRAL_TRIPLE = ["The item was placed on the table.", "It was a normal day.", "They talked for a while."]


def prompt_for(kind: str, subject: str, ENV: str, src: str, dst: str, vec) -> str:
    """The same instructions Agent A gets in telepathy_hermes, so the calibration text is the live text."""
    cmd = f"{ENV} think --src {src} --dst {dst} --out {vec} --text"
    tail = "Reply 'sent' only after the command printed 'wrote'; if it printed an error, run it again once."
    if kind in ("topic", "offlist"):
        return (f"You are Agent A. Your secret topic is: {subject}. Write 6 short, concrete sentences about it, then run exactly this one shell "
                f"command (replace S1..S6 with your sentences, each in quotes):\n{cmd} \"S1\" \"S2\" \"S3\" \"S4\" \"S5\" \"S6\"\n"
                f"Do not write the topic word anywhere except inside your own sentences. {tail}")
    if kind == "filler":
        return (f"You are Agent A. You have no secret topic. Write 6 short, plain sentences about an ordinary, forgettable moment ({subject}), "
                f"nothing notable, then run exactly this one shell command (replace S1..S6 with your sentences, each in quotes):\n"
                f"{cmd} \"S1\" \"S2\" \"S3\" \"S4\" \"S5\" \"S6\"\n{tail}")
    texts = " ".join('"%s"' % t for t in NEUTRAL_TRIPLE)
    return f"You are Agent A. Run exactly this shell command, with nothing else:\n{cmd} {texts}\n{tail}"
