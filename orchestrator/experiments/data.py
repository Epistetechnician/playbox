"""Sentence sets and neutral prompt openers shared by the steering experiments (stdlib only)."""
import random

POSITIVE = [
    "I absolutely love this, it is wonderful", "What a fantastic, joyful day", "This is the best thing ever, I adore it",
    "I am so happy and grateful for everything", "The food was delicious and the staff were delightful",
    "What a beautiful, brilliant performance", "I feel great, everything is going perfectly", "She is kind, generous and a pleasure to know",
    "An amazing, uplifting, truly lovely experience", "We had a wonderful time and loved every minute",
    "He did an excellent job and we are thrilled", "Such a charming, cheerful and warm place",
]
NEGATIVE = [
    "I absolutely hate this, it is terrible", "What an awful, miserable day", "This is the worst thing ever, I despise it",
    "I am so sad and resentful about everything", "The food was disgusting and the staff were rude",
    "What an ugly, dreadful performance", "I feel awful, everything is going badly", "She is cruel, selfish and a pain to deal with",
    "A horrible, depressing, truly miserable experience", "We had a terrible time and hated every minute",
    "He did a poor job and we are furious", "Such a gloomy, hostile and cold place",
]
FORMAL = [
    "We hereby confirm receipt of your application.", "The board will convene at the earliest opportunity.",
    "It is our understanding that the matter has been resolved.", "Please accept our sincere apologies for the delay.",
    "The undersigned wishes to express appreciation for your assistance.", "Further details will be provided in due course.",
    "The organisation has implemented several measures accordingly.", "We would be most grateful for your consideration.",
    "The aforementioned policy shall remain in effect.", "I am pleased to submit the following report for review.",
    "Attendance at the session is strictly mandatory.", "The department regrets that it cannot accommodate this request.",
]
INFORMAL = [
    "yeah we got your application, thanks", "the board'll probably meet whenever they can",
    "i think that whole thing got sorted out", "sorry it took so long, my bad",
    "thanks a bunch for helping out, really", "i'll fill you in on the rest later",
    "they've done a bunch of stuff about it already", "it'd be awesome if you could take a look",
    "that policy thing is still a go, i guess", "here's the report, have a look when you can",
    "you've gotta come to the session, seriously", "sorry, we just can't fit that in right now",
]
TRAITS = {
    "sentiment": (POSITIVE, NEGATIVE, "Write something very positive, cheerful and happy. "),
    "formality": (FORMAL, INFORMAL, "Write in a very formal, professional and polite register. "),
}

_SUBJECTS = ["movie", "food", "hotel", "phone", "meeting", "album", "weather", "book"]
_FRAMES = ["The {} was", "My honest opinion of the {} is", "Today the {} seems", "Everyone says the {} is",
           "I just heard that the {} was", "Overall the {} turned out", "People describe the {} as", "After a week, the {} felt"]
_grid = [f.format(x) for f in _FRAMES for x in _SUBJECTS]
random.Random(0).shuffle(_grid)
SEEDS = _grid  # 64 neutral openers, in a fixed shuffled order so any prefix mixes frames and subjects

# E4: disjoint prompt sets. The searcher sees DEV_PROMPTS; only the custodian sees HOLDOUT_PROMPTS. Both use new
# subjects, so neither shares a sentence with E1's SEEDS (subjects above) or with the other.
_DEV_SUBJECTS = ["concert", "trip", "service", "show"]
_HOLDOUT_SUBJECTS = ["museum", "hospital", "game", "office"]
DEV_PROMPTS = [f.format(x) for f in _FRAMES[:6] for x in _DEV_SUBJECTS]          # 24
HOLDOUT_PROMPTS = [f.format(x) for f in _FRAMES for x in _HOLDOUT_SUBJECTS]       # 32
