"""Held-out pairs for formality: (more formal, less formal). None appear in the extraction sets of
experiments/e1_gpt2.py. Some are mild, since steered GPT-2 text is rarely extreme."""

GOLD = [
    ("We regret to inform you that the meeting has been postponed.", "sorry guys, the meeting got pushed back"),
    ("Please find the requested documents attached.", "here's the stuff you asked for"),
    ("I would appreciate your prompt response to this inquiry.", "get back to me soon, ok?"),
    ("The committee has reached a unanimous decision.", "the committee all agreed, basically"),
    ("It is with great pleasure that we announce the new appointment.", "we're super happy to say we hired someone new"),
    ("The results indicate a significant improvement over the baseline.", "the results are way better than before, honestly"),
    ("Kindly refrain from using mobile devices during the lecture.", "don't use your phones in class, please"),
    ("We apologise for any inconvenience this may have caused.", "sorry if that messed things up for you"),
    ("The company will be undertaking a comprehensive review.", "the company's gonna take a good look at everything"),
    ("I am writing to enquire about the availability of the venue.", "hey, is the place free?"),
    ("Participants are requested to arrive no later than nine o'clock.", "everyone should get there by nine, yeah"),
    ("The proposal was submitted in accordance with the guidelines.", "we sent in the proposal like the rules said"),
    ("He expressed his gratitude to the members of the board.", "he thanked the board folks a ton"),
    ("The apartment is located within walking distance of the station.", "the flat's like a short walk from the station"),
    ("Please be advised that the office will be closed on Monday.", "heads up, the office is shut on monday"),
    ("The manager has not yet responded to our request.", "the boss still hasn't gotten back to us"),
    ("We would like to schedule a consultation at your earliest convenience.", "can we set up a chat sometime soon"),
    ("The report concludes that further investigation is warranted.", "the report says we should dig into it more"),
    ("Employees must submit their expense claims by the end of the month.", "you gotta hand in your expenses by month end"),
    ("I do not believe this approach is advisable.", "i dont think this is a good idea tbh"),
    ("The delegation arrived in the capital on Tuesday evening.", "the delegates got to the capital tuesday night"),
    ("Thank you for your correspondence of the fourth of March.", "thanks for your email from march 4th"),
    ("It would be inappropriate to comment on pending litigation.", "i really shouldn't talk about the court case"),
    ("The terms of the agreement are set out below.", "here's what the deal says"),
]
