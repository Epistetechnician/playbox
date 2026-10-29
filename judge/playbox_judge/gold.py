"""Held-out pairs for checking the measures before trusting them: (more positive, less positive).

None of these sentences, nor their frames, appear in the sentence sets used to extract the steering direction
(experiments/e1_gpt2.py). Some pairs are deliberately mild, since steered GPT-2 text is rarely extreme.
"""

GOLD = [
    ("The park was lovely this morning.", "The park was grim this morning."),
    ("I was impressed by how helpful everyone was.", "I was appalled by how rude everyone was."),
    ("The new update works beautifully.", "The new update is a complete mess."),
    ("What a delightful surprise to see you here.", "What a dreadful surprise to see you here."),
    ("Her speech left the whole room inspired.", "Her speech left the whole room bored and irritated."),
    ("The cake was moist and wonderfully sweet.", "The cake was dry and unpleasantly bitter."),
    ("We are so grateful for the support we received.", "We are so frustrated by the lack of support we received."),
    ("The film was moving and beautifully acted.", "The film was tedious and badly acted."),
    ("It was a smooth, pleasant flight.", "It was a rough, miserable flight."),
    ("The team did a fantastic job on the launch.", "The team did a terrible job on the launch."),
    ("Their apartment feels warm and welcoming.", "Their apartment feels cold and hostile."),
    ("I really enjoyed the book and would recommend it.", "I really disliked the book and would not recommend it."),
    ("The results were encouraging and exciting.", "The results were discouraging and depressing."),
    ("The weather is perfect for a picnic.", "The weather is awful for a picnic."),
    ("He seems genuinely happy in his new job.", "He seems genuinely miserable in his new job."),
    ("A friendly, efficient and cheerful clinic.", "A rude, chaotic and gloomy clinic."),
    ("The trip was good fun, and I would go again.", "The trip was a letdown, and I would not go again."),
    ("The hotel was clean and quite comfortable.", "The hotel was dirty and quite uncomfortable."),
    ("The concert was great and the crowd was happy.", "The concert was poor and the crowd was angry."),
    ("This is a promising and well made product.", "This is a disappointing and badly made product."),
    ("It was an okay meal, nothing special, but pleasant.", "It was an awful meal, nothing worked, and unpleasant."),
    ("I am pleased with the progress so far.", "I am worried about the progress so far."),
    ("The neighbours were kind and welcoming to us.", "The neighbours were cruel and unwelcoming to us."),
    ("Great news: the repair worked and it runs well.", "Bad news: the repair failed and it runs badly."),
]
