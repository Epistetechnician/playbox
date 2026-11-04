"""Fifteen more test concepts in natural, untemplated sentences, for checking the hub beyond five concepts and beyond the
template anchors (docs/latent-channels.md 6k). Same shape as `TEST_CONCEPTS`: eight sentences the sender reads (they build
the thought vector) and a lexicon the decoder counts (written separately, sharing few words with the sentences). None of
these is in any anchor set: `natural_anchors` drops every sentence that contains a lexicon word."""
import re

from .telepathy_data import TEST_CONCEPTS

NATURAL_CONCEPTS = {
    "volcano": {
        "sentences": [
            "Ash rose in a dark column above the crater and drifted for miles downwind.",
            "Molten rock poured down the mountainside, burning everything in its path.",
            "Scientists recorded a swarm of small tremors before the eruption began.",
            "The villagers fled as the slopes began to glow orange at night.",
            "A thick layer of pumice covered the town for centuries afterward.",
            "Sulfur fumes seeped from cracks near the summit and stung their eyes.",
            "The cone collapsed, sending a burning cloud racing into the valley.",
            "Islands formed over millions of years where the magma reached the surface.",
        ],
        "lexicon": ["volcano", "volcanoes", "volcanic", "lava", "magma", "eruption", "erupted", "erupt", "crater", "ash", "ashes", "tectonic", "geothermal", "vent", "caldera", "pyroclastic", "seismic", "tremor", "tremors", "molten", "vesuvius", "etna", "krakatoa", "fumarole", "basalt", "geologist", "geologists", "plate", "plates", "rift"],
    },
    "cooking": {
        "sentences": [
            "She chopped the onions finely and let them soften in butter over a low flame.",
            "The dough needs to rest for an hour before it is rolled out and baked.",
            "A pinch of salt and a squeeze of lemon brightened the whole sauce.",
            "He simmered the stock for hours until the bones gave up all their flavor.",
            "Season the meat generously, then sear it until a brown crust forms.",
            "Fold the egg whites gently so the batter stays light and airy.",
            "The kitchen smelled of garlic, roasted peppers and fresh bread.",
            "Serve the soup hot with a spoonful of cream and chopped herbs on top.",
        ],
        "lexicon": ["cook", "cooking", "cooked", "chef", "chefs", "recipe", "recipes", "kitchen", "oven", "bake", "baking", "baked", "boil", "boiling", "fry", "frying", "roast", "roasted", "grill", "ingredient", "ingredients", "sauce", "soup", "spice", "spices", "flour", "sugar", "butter", "dish", "dishes", "meal", "meals", "cuisine", "delicious", "tasty", "flavor"],
    },
    "finance": {
        "sentences": [
            "The central bank raised interest rates for the third time this year.",
            "Shares fell sharply after the company reported weaker earnings than expected.",
            "Investors moved their savings into government bonds as uncertainty grew.",
            "The firm borrowed heavily to expand and struggled to repay its creditors.",
            "Inflation eroded the value of deposits held in ordinary accounts.",
            "Analysts expect the currency to weaken against the dollar next quarter.",
            "A merger of the two lenders created the largest bank in the region.",
            "Mortgage payments rose as borrowing costs climbed across the market.",
        ],
        "lexicon": ["finance", "financial", "bank", "banks", "banking", "stock", "stocks", "market", "markets", "invest", "investor", "investors", "investment", "profit", "profits", "loan", "loans", "credit", "debt", "interest", "inflation", "economy", "economic", "currency", "dollar", "dollars", "money", "fund", "funds", "capital", "trading", "trader", "wall", "budget", "tax", "taxes"],
    },
    "law": {
        "sentences": [
            "The judge instructed the jury to consider only the evidence presented in court.",
            "Her lawyer argued that the contract was void because it had been signed under pressure.",
            "The court overturned the earlier ruling and ordered a new trial.",
            "Witnesses described the events of that night under cross-examination.",
            "The defendant pleaded not guilty to all charges brought by the prosecution.",
            "A new statute narrowed the circumstances in which police may search a home.",
            "The appeal was heard by a panel of three senior judges.",
            "Damages were awarded after the tribunal found the employer in breach.",
        ],
        "lexicon": ["law", "laws", "legal", "court", "courts", "judge", "judges", "jury", "lawyer", "lawyers", "attorney", "trial", "verdict", "sentence", "guilty", "innocent", "crime", "criminal", "prosecutor", "prosecution", "defendant", "plaintiff", "appeal", "statute", "legislation", "constitution", "constitutional", "justice", "lawsuit", "sue", "evidence", "testimony", "witness", "witnesses"],
    },
    "war": {
        "sentences": [
            "The army crossed the river at dawn and advanced on the fortified town.",
            "Artillery pounded the lines for three days before the infantry went forward.",
            "Thousands of soldiers were killed in the campaign, and many more were taken prisoner.",
            "The general ordered a retreat when supplies ran short in the winter.",
            "Bombers struck the harbor, sinking several ships at anchor.",
            "The two nations signed an armistice after years of bitter fighting.",
            "Trenches stretched for hundreds of miles across the ravaged countryside.",
            "Reinforcements arrived too late to prevent the siege from succeeding.",
        ],
        "lexicon": ["war", "wars", "battle", "battles", "army", "armies", "soldier", "soldiers", "military", "troops", "weapon", "weapons", "invasion", "invade", "invaded", "enemy", "enemies", "combat", "fight", "fighting", "general", "generals", "navy", "bomb", "bombs", "missile", "missiles", "attack", "attacked", "conflict", "casualties", "veteran", "veterans", "frontline"],
    },
    "religion": {
        "sentences": [
            "Pilgrims walked for weeks to reach the shrine and pray at its altar.",
            "The congregation sang hymns as the priest lit candles before the service.",
            "Monks in the monastery rose before dawn to chant and meditate.",
            "Scholars debated the meaning of the sacred text for generations.",
            "The festival marks the day the prophet is said to have received his revelation.",
            "She kept the fast faithfully and gave alms to the poor each week.",
            "The cathedral's bells rang out across the old quarter on Sunday.",
            "Believers gathered at the temple to offer prayers for the departed.",
        ],
        "lexicon": ["god", "gods", "religion", "religious", "church", "churches", "temple", "mosque", "prayer", "prayers", "pray", "faith", "holy", "sacred", "bible", "quran", "priest", "priests", "bishop", "monk", "monks", "worship", "spiritual", "heaven", "sin", "soul", "christian", "muslim", "jewish", "buddhist", "hindu", "scripture", "divine", "saint", "saints"],
    },
    "computing": {
        "sentences": [
            "The program crashed because a variable was read before it had been assigned.",
            "Engineers rewrote the code to cut the time the server spent on each request.",
            "She trained a neural network on millions of labelled photographs.",
            "The update fixed a security flaw that let attackers run commands remotely.",
            "Data is stored in tables and retrieved with queries written in a special language.",
            "The compiler turns the source files into instructions the processor can execute.",
            "Packets travel across the network and are reassembled at their destination.",
            "He debugged the script line by line until the error finally disappeared.",
        ],
        "lexicon": ["computer", "computers", "software", "hardware", "program", "programs", "programming", "programmer", "code", "coding", "algorithm", "algorithms", "data", "database", "network", "networks", "internet", "web", "server", "servers", "digital", "online", "app", "apps", "bug", "bugs", "cpu", "memory", "laptop", "technology", "tech", "developer", "developers", "cyber", "encryption"],
    },
    "aviation": {
        "sentences": [
            "The pilot throttled back and lowered the flaps as the runway came into view.",
            "Passengers fastened their seatbelts while the cabin crew checked the overhead bins.",
            "Turbulence over the mountains rattled the small aircraft for several minutes.",
            "The control tower cleared the flight for takeoff despite the crosswinds.",
            "An engine failure forced the jet to make an emergency landing at the nearest field.",
            "The airline added three new routes and ordered a dozen long-range planes.",
            "She watched the wings tilt as the plane banked toward the coast.",
            "Delays piled up at the terminal after fog closed the airport for hours.",
        ],
        "lexicon": ["airplane", "airplanes", "aircraft", "plane", "planes", "jet", "jets", "pilot", "pilots", "flight", "flights", "fly", "flying", "airport", "airports", "airline", "airlines", "runway", "cockpit", "wing", "wings", "aviation", "boeing", "airbus", "takeoff", "landing", "altitude", "propeller", "helicopter", "flew", "passenger", "passengers", "hangar"],
    },
    "painting": {
        "sentences": [
            "The artist mixed pigments on her palette and laid down a wash of pale blue.",
            "Visitors lingered in front of the canvas, studying the brushwork up close.",
            "He sketched the model in charcoal before committing any color to the panel.",
            "The gallery hung portraits and landscapes from the seventeenth century side by side.",
            "A restorer cleaned centuries of varnish from the darkened masterpiece.",
            "Her exhibition featured bold abstract works in oil and acrylic.",
            "The sculptor chiseled the marble until the figure seemed ready to breathe.",
            "Critics praised the way light fell across the folds of the painted robe.",
        ],
        "lexicon": ["painting", "paintings", "painter", "painters", "paint", "painted", "art", "arts", "artist", "artists", "canvas", "gallery", "museum", "sculpture", "sculptor", "portrait", "portraits", "landscape", "exhibition", "easel", "brush", "brushes", "oil", "watercolor", "drawing", "sketch", "masterpiece", "renaissance", "impressionist", "curator", "studio", "palette", "mural"],
    },
    "farming": {
        "sentences": [
            "The family planted wheat in the lower fields and kept cattle on the hill.",
            "A dry spring left the soil cracked and the young crops struggling.",
            "At harvest, neighbors came together to bring in the grain before the rains.",
            "He drove the tractor along the rows, turning over the stubble from last year.",
            "The orchard produced more apples than the market could absorb that autumn.",
            "Sheep grazed on the pasture while the farmer mended the fence.",
            "Irrigation channels carried water to the thirsty rows of corn.",
            "Prices for milk fell, and many small dairies could not carry on.",
        ],
        "lexicon": ["farm", "farms", "farmer", "farmers", "farming", "agriculture", "agricultural", "crop", "crops", "harvest", "harvests", "livestock", "cattle", "cow", "cows", "sheep", "pig", "pigs", "chicken", "chickens", "barn", "tractor", "plow", "plough", "soil", "seed", "seeds", "grain", "wheat", "corn", "rural", "ranch", "dairy", "orchard", "pasture"],
    },
    "weather": {
        "sentences": [
            "A cold front swept in overnight, bringing heavy rain and gusting winds.",
            "The forecast promised clear skies and a gentle breeze for the weekend.",
            "Thick fog lay over the valley until the sun burned it off near noon.",
            "Snow piled up on the roads, and the plows could barely keep pace.",
            "Lightning split the sky, and thunder shook the windows seconds later.",
            "An unusually hot summer broke records and left reservoirs low.",
            "The hurricane weakened to a tropical storm as it moved inland.",
            "Frost covered the grass at dawn, and the puddles had frozen solid.",
        ],
        "lexicon": ["weather", "forecast", "forecasts", "rain", "rainy", "snow", "snowy", "storm", "storms", "wind", "winds", "windy", "cloud", "clouds", "cloudy", "sunny", "temperature", "temperatures", "climate", "humid", "humidity", "drought", "flood", "floods", "hurricane", "tornado", "thunder", "lightning", "fog", "frost", "freezing", "meteorologist", "degrees", "monsoon"],
    },
    "railways": {
        "sentences": [
            "The locomotive hissed and pulled out of the station with a long whistle.",
            "Workers laid new track across the plain, spiking each rail to the sleepers.",
            "Commuters packed the platform, waiting for the delayed morning service.",
            "The line climbed through tunnels and over viaducts to reach the high pass.",
            "A signal failure stopped the express outside the junction for an hour.",
            "Freight wagons loaded with coal rolled slowly toward the port.",
            "The company electrified the main line and retired its last steam engines.",
            "He bought a ticket and found a seat by the window in the third carriage.",
        ],
        "lexicon": ["train", "trains", "railway", "railways", "railroad", "railroads", "rail", "rails", "locomotive", "locomotives", "station", "stations", "track", "tracks", "platform", "carriage", "carriages", "conductor", "ticket", "tickets", "subway", "metro", "tram", "trams", "commuter", "commuters", "engine", "express", "freight", "junction", "steam", "diesel"],
    },
    "chemistry": {
        "sentences": [
            "The solution turned blue when the copper salt dissolved in the acid.",
            "Electrons are shared between the two atoms, forming a stable bond.",
            "She added a catalyst to speed up the reaction without changing its product.",
            "The compound is insoluble in water but dissolves readily in alcohol.",
            "Heating the mixture drove off the solvent and left a white crystalline residue.",
            "A titration revealed the concentration of the unknown base.",
            "The element sits in the second column of the periodic table and burns with a bright flame.",
            "Polymers are long chains built from repeating molecular units.",
        ],
        "lexicon": ["chemistry", "chemical", "chemicals", "chemist", "chemists", "molecule", "molecules", "molecular", "atom", "atoms", "atomic", "element", "elements", "compound", "compounds", "reaction", "reactions", "acid", "acids", "base", "bases", "ion", "ions", "oxygen", "hydrogen", "carbon", "nitrogen", "laboratory", "lab", "reagent", "solvent", "solution", "periodic", "synthesis", "polymer"],
    },
    "elections": {
        "sentences": [
            "Voters queued outside polling stations from early morning to cast their ballots.",
            "The incumbent conceded after the count showed her opponent well ahead.",
            "Candidates crisscrossed the country in the final week of the campaign.",
            "Turnout was the highest in thirty years, officials said on Tuesday.",
            "The opposition won a majority of seats in the lower house.",
            "A recount was ordered in two districts where the margin was only a few hundred votes.",
            "The party chose its leader at a convention attended by delegates from every state.",
            "Debates between the rivals focused on taxes, health care and immigration.",
        ],
        "lexicon": ["election", "elections", "elect", "elected", "vote", "votes", "voter", "voters", "voting", "ballot", "ballots", "candidate", "candidates", "campaign", "campaigns", "poll", "polls", "polling", "president", "presidential", "senate", "senator", "congress", "parliament", "political", "politics", "politician", "politicians", "democrat", "republican", "party", "parties", "government", "mayor", "majority"],
    },
    "architecture": {
        "sentences": [
            "The facade was clad in pale limestone and crowned by a copper dome.",
            "Architects designed the tower with a steel frame and glass curtain walls.",
            "Flying buttresses carried the weight of the vaulted ceiling outward to the piers.",
            "The renovation preserved the Victorian staircase while opening up the ground floor.",
            "A colonnade of slender columns lined the courtyard on three sides.",
            "The planners proposed a pedestrian square surrounded by low brick buildings.",
            "Its pointed arches and tall windows are typical of the late medieval style.",
            "The bridge's graceful span was engineered to withstand strong winds and heavy loads.",
        ],
        "lexicon": ["architecture", "architectural", "architect", "architects", "building", "buildings", "design", "designed", "tower", "towers", "facade", "dome", "column", "columns", "arch", "arches", "gothic", "baroque", "skyscraper", "skyscrapers", "cathedral", "construction", "blueprint", "blueprints", "structure", "structures", "foundation", "ceiling", "ceilings", "brick", "bricks", "marble", "concrete", "steel", "urban"],
    },
}

# every concept the natural experiment scores: the original five plus these fifteen
ALL_CONCEPTS = {**TEST_CONCEPTS, **NATURAL_CONCEPTS}


def lexicon_words() -> set[str]:
    return {w for c in ALL_CONCEPTS.values() for w in c["lexicon"]}


def natural_anchors(texts: list[str], n: int, seed: int = 0) -> list[str]:
    """`n` natural sentences from `texts` (raw paragraphs, e.g. Wikipedia): 6 to 30 words, no markup, none that contains a word
    from any concept's lexicon (so no test concept appears in the anchors), none that is a test sentence. Deterministic."""
    import random

    bad = lexicon_words()
    test = {s for c in ALL_CONCEPTS.values() for s in c["sentences"]}
    out, seen = [], set()
    for para in texts:
        if para.lstrip().startswith("=") or "@" in para:
            continue
        for sent in re.split(r"(?<=[.!?]) (?=[A-Z])", para.strip()):
            words = re.findall(r"[A-Za-z']+", sent)
            if not 6 <= len(words) <= 30 or sent in seen or sent in test:
                continue
            if any(w.lower() in bad for w in words) or any(ch in sent for ch in "<>{}|\\"):
                continue
            seen.add(sent)
            out.append(sent.strip())
    random.Random(seed).shuffle(out)
    return out[:n]
