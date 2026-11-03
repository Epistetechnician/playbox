"""Forty more fit topics (none of the 20 evaluation topics or their close neighbours), same template style but four more
sentence frames. Fit-only text: the evaluation topics are never in it."""
import random

from .telepathy_data import FIT_TOPICS, _make

_T2 = [
    "Nobody expected the {n} to look so {a}, yet the {n2} {v} just the same.",
    "After a long wait, the {a} {n} {v} while the {n2} stayed put.",
    "She pointed at the {n} and said the {n2} had {v} again.",
    "In the end it was the {a} {n2}, not the {n}, that {v}.",
]

_BIG = """bakery|oven,loaf,flour,pastry,tray,apron,counter,croissant|warm,golden,crusty,sweet|rose,cooled,sold,baked
desert|dune,cactus,canyon,oasis,sand,lizard,mirage,caravan|dry,vast,scorching,empty|shimmered,shifted,stretched,faded
jungle|vine,canopy,monkey,parrot,swamp,fern,trail,mist|dense,humid,loud,green|buzzed,tangled,rustled,dripped
castle|tower,drawbridge,knight,moat,banner,dungeon,gate,throne|ancient,grim,grand,cold|loomed,crumbled,stood,echoed
circus|tent,clown,trapeze,juggler,ringmaster,elephant,balloon,ticket|colorful,loud,daring,silly|tumbled,cheered,spun,dazzled
prison|cell,guard,corridor,bars,warden,yard,lock,uniform|grim,narrow,locked,silent|clanged,waited,echoed,dragged
bank|vault,teller,account,loan,ledger,counter,safe,deposit|secure,formal,busy,quiet|opened,closed,counted,waited
railway|track,signal,carriage,conductor,tunnel,whistle,timetable,crossing|long,rusty,punctual,steady|rattled,halted,rolled,hissed
forest|oak,moss,deer,path,owl,stump,clearing,thicket|dark,quiet,tall,damp|rustled,swayed,creaked,thinned
cave|stalactite,echo,torch,tunnel,bat,pool,chamber,ledge|dark,damp,narrow,cold|dripped,echoed,narrowed,glowed
theater|stage,curtain,actor,script,spotlight,balcony,aisle,costume|dramatic,packed,dim,grand|rose,fell,applauded,flickered
museum|exhibit,statue,guide,gallery,display,painting,vase,ticket|hushed,old,priceless,spacious|gleamed,stood,watched,waited
zoo|cage,giraffe,keeper,penguin,enclosure,tiger,feed,path|crowded,curious,loud,playful|roared,paced,stared,splashed
supermarket|aisle,cart,shelf,cashier,coupon,freezer,bag,receipt|bright,crowded,cheap,endless|beeped,rolled,emptied,queued
restaurant|menu,waiter,table,chef,bill,napkin,dessert,reservation|elegant,busy,spicy,cozy|arrived,sizzled,filled,closed
bridge|cable,arch,pillar,span,toll,railing,tower,deck|long,high,narrow,sturdy|swayed,spanned,trembled,carried
factory|conveyor,furnace,worker,shift,machine,smokestack,crate,whistle|noisy,huge,grimy,efficient|hummed,clanged,ran,stopped
mine|shaft,pickaxe,cart,lantern,ore,tunnel,miner,beam|deep,dusty,cramped,dark|collapsed,echoed,rumbled,glittered
university|lecture,campus,professor,dorm,thesis,exam,quad,seminar|crowded,serious,ancient,lively|began,dragged,ended,echoed
laboratory|beaker,microscope,sample,notebook,flask,goggles,bench,pipette|clean,cluttered,precise,quiet|bubbled,hummed,cooled,clicked
battlefield|trench,banner,drum,bugle,shield,rampart,horse,smoke|bloody,muddy,silent,wide|thundered,charged,fell,rang
wedding|bride,ring,cake,aisle,vow,bouquet,guest,toast|joyful,elegant,tearful,long|danced,cheered,kissed,began
birthday|candle,gift,balloon,cake,card,party,wish,song|happy,loud,sweet,surprising|glowed,popped,cheered,arrived
camping|tent,campfire,sleeping bag,lantern,trail,marshmallow,stream,backpack|cold,smoky,muddy,starry|crackled,flapped,rained,glowed
gym|barbell,treadmill,mirror,locker,bench,towel,trainer,mat|sweaty,crowded,loud,bright|clanged,hummed,lifted,emptied
salon|scissors,chair,mirror,dryer,shampoo,comb,stylist,towel|fancy,busy,fragrant,bright|snipped,buzzed,swept,gleamed
hotel|lobby,suite,bellhop,elevator,key,reception,balcony,minibar|luxurious,quiet,crowded,plush|opened,chimed,filled,waited
police|siren,badge,patrol,report,radio,handcuff,station,detective|urgent,tense,tired,sharp|wailed,chased,arrived,filed
firehouse|hose,ladder,helmet,truck,alarm,pole,siren,crew|red,brave,loud,quick|rang,raced,sprayed,climbed
postoffice|stamp,parcel,mailbox,envelope,clerk,scale,counter,letter|slow,crowded,dusty,orderly|arrived,weighed,stamped,piled
bookstore|novel,shelf,cashier,bestseller,bookmark,cover,aisle,sale|cozy,quiet,crowded,musty|rustled,sold,piled,waited
toyshop|doll,robot,puzzle,train,kite,teddy,window,box|colorful,tiny,shiny,noisy|rattled,squeaked,spun,sparkled
petshop|puppy,cage,leash,aquarium,hamster,collar,bowl,parrot|furry,noisy,cute,playful|barked,chirped,splashed,squeaked
vineyard|grape,barrel,cellar,vine,harvest,cork,hillside,press|ripe,sunny,old,rolling|ripened,fermented,swayed,gleamed
greenhouse|seedling,glass,tray,pot,sprinkler,tomato,bench,vent|humid,warm,bright,crowded|sprouted,dripped,fogged,thrived
snowfall|drift,sled,glove,icicle,shovel,flake,scarf,chimney|white,cold,soft,deep|fell,melted,piled,glittered
autumn|leaf,pumpkin,harvest,acorn,sweater,orchard,bonfire,scarecrow|golden,crisp,windy,red|fell,crunched,turned,swirled
lake|dock,canoe,cabin,heron,shore,fog,ripple,lily|calm,still,cool,glassy|rippled,reflected,froze,shimmered
village|chapel,well,cottage,lane,baker,green,fountain,gossip|quiet,tiny,quaint,sleepy|slept,woke,gathered,stood
harbor_market|crate,scale,awning,cart,basket,vendor,barrel,scarf|noisy,fragrant,busy,crowded|bustled,haggled,spilled,sold
"""

_ROWS = [r.split("|") for r in _BIG.strip().splitlines()]


def _make2(topic: str, nouns, adjs, verbs, k: int = 36) -> list[str]:
    r = random.Random("big-" + topic)
    out = []
    for _ in range(k):
        n, n2 = r.sample(nouns, 2)
        out.append(r.choice(_T2).format(a=r.choice(adjs), n=n, n2=n2, v=r.choice(verbs)))
    return out


EXTRA_TOPICS = {t.replace("_", " "): _make2(t, n.split(","), a.split(","), v.split(",")) for t, n, a, v in _ROWS}
FIT_TOPICS_BIG = {**FIT_TOPICS, **EXTRA_TOPICS}
