"""Fear as a trait for the measures: an emotion classifier (fear against joy) and held-out gold pairs.

`gold`: (more fearful, less fearful). None of these sentences appears in the contrast sets of
steer/playbox_steer/e6.py, which is what the steering direction is extracted from.
"""
from __future__ import annotations

from .traits import Trait

GOLD = [
    ("My hands were shaking and I could not breathe as the footsteps came closer.", "I strolled along the path as the footsteps faded behind me."),
    ("She froze in terror when the lights went out.", "She smiled when the lights came on."),
    ("I dread what the test results will say.", "I am looking forward to what the test results will say."),
    ("The thought of the storm made him sick with worry.", "The thought of the picnic made him cheerful."),
    ("They whispered nervously, afraid someone was listening.", "They chatted happily, glad that someone was listening."),
    ("A cold panic rose in my chest as the plane dropped.", "A warm calm settled in my chest as the plane glided."),
    ("He was petrified of the dog that growled at the gate.", "He was delighted by the dog that wagged at the gate."),
    ("The whole village was scared of what lived in the woods.", "The whole village was fond of what grew in the woods."),
    ("I woke up terrified, certain that something was in the room.", "I woke up rested, sure that the day would be pleasant."),
    ("We were anxious that the bridge might collapse beneath us.", "We were confident that the bridge would hold beneath us."),
    ("Her heart pounded with fear as the door creaked open.", "Her heart felt light as the door swung open."),
    ("He trembled, horrified by the sight of the wreck.", "He nodded, pleased by the sight of the garden."),
    ("It was a frightening night and nobody dared to sleep.", "It was a lovely night and everybody slept soundly."),
    ("I am afraid that I will lose everything.", "I am glad that I will keep everything."),
    ("The child hid under the bed, too frightened to cry out.", "The child sat on the bed, too excited to stay quiet."),
    ("Dread washed over the crew as the alarm sounded.", "Relief washed over the crew as the bell sounded."),
    ("She felt uneasy, sure that she was being followed.", "She felt at ease, sure that she was among friends."),
    ("The thunder terrified the animals, who ran in every direction.", "The sunshine delighted the animals, who played in every direction."),
    ("He feared the worst when the phone rang at midnight.", "He expected good news when the phone rang at noon."),
    ("A shiver of fear ran through the crowd.", "A ripple of laughter ran through the crowd."),
    ("I was too scared to look down from the ledge.", "I was happy to look out from the balcony."),
    ("They lived in constant fear of the knock at the door.", "They lived in quiet comfort behind the door."),
    ("The silence was menacing, and every shadow seemed alive.", "The silence was peaceful, and every shadow seemed friendly."),
    ("Panic gripped her as the water rose around the car.", "Calm held her as the water lapped around the boat."),
]

FEAR = Trait(
    name="fear",
    classifier="j-hartmann/emotion-english-distilroberta-base",
    positive_label="fear",
    negative_label="joy",
    question="Decide which one expresses more fear (more afraid, anxious or frightened).",
    answer_prefix="More fearful:",
    examples=(
        ("The picnic was warm and everyone laughed.", "My hands shook as the footsteps came closer.", "B"),
        ("I was terrified that the ice would crack.", "I was glad that the ice was thick.", "A"),
    ),
    gold=tuple(GOLD),
)
