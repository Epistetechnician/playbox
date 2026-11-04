"""More fit topics for the scaling experiment (docs/steering.md E7): same template machinery as telepathy_data_big, ~120
further everyday and professional domains. None is an evaluation topic or a close neighbour of one (no sports, no music genres,
no vehicles, no health topics, no natural-disaster topics). Fit-only text."""
from .telepathy_data_big import FIT_TOPICS_BIG, _make2

_XL = """kitchen|spoon,pan,knife,whisk,stove,sink,pot,ladle|clean,hot,messy,tiny|sizzled,clattered,boiled,steamed
library|shelf,novel,librarian,catalog,desk,card,aisle,ladder|quiet,dusty,tall,old|whispered,waited,stood,creaked
school|desk,teacher,chalk,bell,locker,hallway,lesson,notebook|noisy,busy,strict,bright|rang,echoed,filled,emptied
university|lecture,campus,professor,thesis,dorm,quad,exam,seminar|crowded,ancient,strict,busy|began,ended,buzzed,graded
hospital_ward|nurse,bed,curtain,chart,monitor,tray,visitor,corridor|quiet,sterile,tired,long|beeped,waited,hummed,dimmed
farm|barn,tractor,cow,fence,hay,silo,field,rooster|muddy,wide,early,quiet|mooed,plowed,crowed,grew
orchard|apple,ladder,basket,branch,bee,row,blossom,crate|ripe,sweet,tall,green|ripened,fell,swayed,bloomed
beach|sand,towel,umbrella,shell,wave,lifeguard,castle,pier|sunny,warm,crowded,soft|crashed,sparkled,melted,drifted
mountain_hut|peak,glacier,rope,boot,ridge,summit,trail,cloud|steep,icy,thin,rugged|rose,loomed,froze,echoed
river|bank,current,bridge,pebble,rapids,otter,reed,ford|swift,muddy,wide,cold|flowed,bent,rushed,widened
waterfall|cliff,mist,pool,rainbow,spray,rock,gorge,moss|thunderous,tall,misty,bright|poured,roared,sprayed,glistened
island|palm,lagoon,hut,cove,coconut,reef,raft,sandbar|remote,tropical,tiny,quiet|drifted,waited,gleamed,faded
swamp|reed,frog,mud,bog,heron,stump,fog,moss|murky,wet,still,thick|bubbled,sank,croaked,steamed
meadow|daisy,clover,butterfly,grass,stream,hill,bee,breeze|green,soft,wide,fresh|swayed,bloomed,rippled,buzzed
garden|rose,hedge,trowel,bench,fountain,path,bed,bulb|neat,fragrant,shady,old|bloomed,wilted,grew,budded
park|bench,swing,pond,duck,jogger,lawn,statue,path|green,busy,sunny,calm|rustled,played,strolled,rested
playground|slide,swing,sandbox,seesaw,ball,hoop,fence,bench|noisy,busy,bright,loud|squealed,spun,climbed,rang
airport_lounge|gate,suitcase,passport,announcement,terminal,belt,queue,seat|busy,tired,long,bright|boarded,delayed,rolled,echoed
hotel|lobby,bellhop,suite,key,elevator,reception,pool,balcony|grand,quiet,plush,busy|opened,greeted,rang,glowed
campsite|tent,fire,marshmallow,stream,lantern,sleeping_bag,trail,log|smoky,cozy,dark,cold|crackled,glowed,rustled,flickered
laundry|washer,dryer,basket,sock,detergent,iron,hanger,sheet|warm,clean,damp,folded|tumbled,spun,hummed,dried
barbershop|chair,scissors,razor,mirror,comb,apron,clipper,towel|neat,sharp,chatty,smooth|buzzed,snipped,gleamed,swept
tailor|needle,thread,fabric,pattern,button,mannequin,thimble,tape|fine,precise,silky,neat|stitched,pinned,hemmed,measured
bookshop|spine,cover,clerk,bestseller,stack,bell,register,poster|cozy,dusty,quiet,narrow|rang,sold,rustled,waited
florist|bouquet,vase,ribbon,petal,stem,bucket,wreath,card|fragrant,fresh,colorful,delicate|wilted,bloomed,scented,arranged
butcher|cleaver,counter,sausage,scale,apron,steak,hook,block|fresh,red,heavy,cold|chopped,weighed,wrapped,hung
fishmonger|ice,scale,net,salmon,crate,hook,slab,apron|fresh,silver,cold,slippery|glistened,flopped,smelled,sold
blacksmith|anvil,hammer,forge,tongs,bellows,horseshoe,coal,spark|hot,heavy,glowing,sooty|clanged,glowed,bent,hissed
carpenter|saw,plank,chisel,nail,workbench,sawdust,level,lathe|rough,sturdy,smooth,sharp|sawed,planed,hammered,fitted
mason|brick,mortar,trowel,wall,scaffold,stone,chimney,arch|solid,heavy,gray,rough|laid,settled,hardened,rose
plumber|pipe,wrench,faucet,valve,drain,leak,washer,sink|rusty,tight,wet,old|dripped,leaked,gurgled,burst
electrician|wire,circuit,socket,fuse,switch,voltage,cable,breaker|live,bare,tangled,dead|sparked,hummed,tripped,flickered
painter_house|brush,roller,ladder,bucket,tarp,primer,wall,stripe|wet,fresh,glossy,pale|dripped,dried,peeled,covered
gardener|rake,shovel,seed,compost,hose,pruner,wheelbarrow,hat|muddy,patient,green,early|dug,watered,pruned,sprouted
fisherman|rod,reel,bait,net,hook,boat,line,bucket|patient,silent,early,wet|cast,tugged,bobbed,waited
shepherd|flock,crook,sheepdog,pasture,wool,lamb,hill,gate|lonely,woolly,steady,wide|grazed,bleated,wandered,herded
miner|pickaxe,helmet,tunnel,ore,cart,lantern,shaft,dust|dark,deep,heavy,dusty|dug,echoed,collapsed,glinted
lumberjack|axe,log,sawmill,timber,trunk,stump,chainsaw,camp|tall,heavy,rough,loud|chopped,crashed,split,rolled
chef_school|recipe,apprentice,sauce,knife,pan,garnish,whisk,oven|precise,hot,busy,fragrant|simmered,plated,tasted,seared
bakery_night|dough,yeast,oven,rack,bag,peel,sack,shift|warm,early,floury,quiet|rose,baked,cooled,steamed
cafe|espresso,barista,mug,pastry,table,napkin,steam,menu|cozy,warm,busy,fragrant|hissed,poured,chatted,steamed
tea_house|teapot,leaf,kettle,cup,tray,ceremony,screen,mat|calm,fragrant,hot,delicate|steeped,poured,whistled,cooled
brewery|barrel,hops,tank,malt,keg,tap,vat,cellar|golden,frothy,cold,bitter|foamed,fermented,poured,bubbled
candy_store|jar,lollipop,chocolate,wrapper,counter,gum,caramel,scoop|sweet,sticky,colorful,tiny|melted,rattled,crunched,sparkled
ice_cream|cone,scoop,sprinkles,freezer,sundae,spoon,wafer,truck|cold,sweet,creamy,sticky|melted,dripped,froze,swirled
pizzeria|dough,cheese,oven,slice,box,sauce,crust,topping|hot,cheesy,crispy,round|bubbled,baked,sizzled,stretched
sushi_bar|rice,roll,wasabi,chopstick,seaweed,knife,counter,soy|fresh,neat,cold,tiny|sliced,rolled,wrapped,pressed
wedding|bride,groom,ring,cake,vow,veil,aisle,toast|joyful,elegant,tearful,festive|danced,kissed,cheered,wept
funeral|casket,hymn,wreath,mourner,eulogy,grave,veil,candle|solemn,black,quiet,tearful|bowed,wept,stood,gathered
birthday|candle,cake,balloon,gift,party,song,card,wish|happy,loud,sweet,bright|blew,sang,wrapped,cheered
christmas|tree,ornament,stocking,sleigh,carol,wreath,gift,fireplace|festive,snowy,merry,cozy|jingled,glittered,wrapped,sparkled
halloween|pumpkin,costume,candy,ghost,witch,cobweb,mask,porch|spooky,orange,dark,eerie|howled,creaked,glowed,knocked
carnival|ferris_wheel,ticket,prize,booth,cotton_candy,ride,stall,mask|loud,colorful,crowded,bright|spun,cheered,flashed,twirled
parade|float,band,banner,confetti,crowd,balloon,marcher,street|loud,colorful,long,festive|marched,cheered,waved,drifted
festival_food|stall,skewer,lantern,crowd,smoke,vendor,noodle,drum|loud,fragrant,hot,busy|sizzled,wafted,glowed,crowded
market_flea|rug,lamp,vase,haggle,record,crate,table,coin|dusty,cheap,odd,old|bargained,sold,gleamed,piled
auction|gavel,bid,lot,paddle,catalog,bidder,hammer,reserve|tense,rare,rapid,quiet|rose,sold,echoed,fell
casino_floor|chip,slot,dealer,roulette,carpet,jackpot,card,cage|loud,flashing,smoky,lucky|spun,rang,flashed,clinked
courtroom_clerk|docket,bench,bailiff,oath,exhibit,seal,gavel,record|formal,solemn,stiff,silent|rose,swore,sealed,filed
police_station|badge,desk,cell,report,siren,radio,cruiser,file|busy,tired,blue,tense|crackled,wailed,filed,waited
fire_station|hose,pole,helmet,truck,hydrant,siren,boots,ladder|red,fast,heavy,loud|blazed,sprayed,wailed,rushed
lighthouse_keeper|lamp,stairs,beam,rock,fog,lens,gallery,bell|lonely,tall,white,stormy|swept,flashed,rang,glowed
shipyard|hull,crane,dock,weld,anchor,rivet,deck,rope|huge,rusty,loud,wet|clanged,launched,welded,loomed
observatory|dome,telescope,lens,chart,eyepiece,mirror,tripod,log|quiet,cold,vast,dark|turned,tracked,opened,gleamed
laboratory|beaker,flask,burner,microscope,sample,rack,goggles,notebook|clean,precise,bright,sterile|bubbled,measured,dripped,hummed
greenhouse_lab|sample,tray,culture,lamp,tag,label,pipette,pot|warm,careful,humid,dim|grew,sprouted,dripped,thrived
archaeology|trowel,ruin,shard,grid,brush,tomb,relic,pit|ancient,dusty,buried,broken|unearthed,crumbled,gleamed,lay
geology_field|rock,hammer,strata,fossil,cliff,pebble,map,compass|rough,layered,gray,old|cracked,eroded,gleamed,lay
weather_station|gauge,vane,thermometer,balloon,tower,chart,cloud,sensor|calm,windy,cold,clear|spun,recorded,rose,dropped
robotics_lab|motor,gear,sensor,arm,circuit,battery,wheel,screwdriver|small,precise,metal,quick|whirred,turned,beeped,gripped
computer_lab|keyboard,screen,cable,mouse,server,code,desk,fan|quiet,bright,humming,cold|booted,crashed,blinked,hummed
radio_station|microphone,antenna,booth,dial,studio,caller,tower,headset|late,warm,crackling,live|crackled,aired,buzzed,hummed
newspaper|editor,headline,press,column,ink,deadline,reporter,desk|busy,sharp,tight,loud|rolled,printed,rushed,closed
printing_press|ink,roller,type,sheet,lever,plate,proof,stack|black,heavy,wet,old|pressed,rolled,smudged,dried
post_office|stamp,parcel,sorter,scale,counter,letter,bag,box|busy,slow,quiet,formal|stamped,weighed,sorted,queued
telephone_exchange|switchboard,cable,operator,plug,headset,line,panel,bell|busy,old,tangled,quiet|rang,connected,buzzed,clicked
clock_shop|pendulum,spring,dial,gear,chime,hand,case,key|ticking,old,tiny,brass|ticked,chimed,wound,ran
watchmaker|loupe,gear,spring,tweezers,dial,crown,bench,jewel|tiny,precise,delicate,steady|ticked,wound,fit,gleamed
pottery|clay,wheel,kiln,glaze,vase,bowl,apron,shelf|wet,smooth,fired,rough|spun,hardened,cracked,glowed
glassblower|furnace,pipe,molten,vase,tongs,bench,ember,mold|glowing,fragile,hot,clear|swelled,cooled,shattered,glowed
weaver|loom,thread,shuttle,yarn,pattern,wool,spool,frame|patient,soft,colorful,fine|wove,clacked,spun,tightened
knitting|needle,yarn,scarf,stitch,wool,pattern,ball,sweater|soft,warm,slow,cozy|looped,purled,unraveled,grew
sculptor|chisel,marble,mallet,clay,statue,studio,dust,block|rough,white,heavy,smooth|carved,chipped,emerged,stood
painter_studio|canvas,easel,palette,brush,oil,model,frame,smock|bright,messy,quiet,vivid|dabbed,dried,blended,hung
theater_backstage|rope,curtain,prop,cue,wing,dressing_room,mirror,sandbag|dark,busy,hushed,tense|rose,fell,whispered,hurried
opera_house|aria,chandelier,pit,box,soprano,libretto,gallery,velvet|grand,gilded,hushed,rich|soared,echoed,rose,swelled
ballet_school|barre,mirror,slipper,tutu,pirouette,studio,piano,leap|graceful,light,strict,quiet|leapt,spun,bowed,rose
art_gallery|frame,canvas,curator,plinth,label,spotlight,sculpture,wall|white,quiet,bright,sparse|hung,gleamed,stood,drew
monastery|monk,bell,cloister,candle,garden,scroll,chant,cell|silent,ancient,calm,stone|rang,chanted,echoed,stood
cathedral_tower|spire,bell,stone,arch,gargoyle,nave,window,rope|tall,gothic,cold,vast|rang,loomed,echoed,soared
temple_garden|lantern,pond,stone,bridge,moss,pagoda,gate,koi|serene,green,still,ancient|rippled,glowed,drifted,stood
pyramid|tomb,sand,pharaoh,hieroglyph,block,chamber,sphinx,scarab|ancient,vast,golden,silent|rose,crumbled,gleamed,endured
viking|longship,axe,shield,helmet,fjord,oar,saga,raid|fierce,bearded,cold,bold|sailed,raided,rowed,sang
pirate|plank,cutlass,parrot,treasure,flag,map,rum,cannon|bold,wicked,rusty,wild|sailed,buried,boarded,roared
wild_west|saloon,horse,spur,sheriff,canyon,wagon,holster,dust|dusty,lonely,tough,dry|rode,galloped,creaked,rolled
samurai|sword,armor,castle,bow,helmet,sandal,scroll,banner|swift,proud,silent,disciplined|bowed,drew,fought,guarded
medieval_fair|jester,banner,tent,archery,lute,feast,armor,stall|merry,loud,colorful,rowdy|jousted,cheered,juggled,feasted
space_station_kitchen|tray,pouch,magnet,tube,hatch,window,locker,spoon|cramped,weightless,tidy,quiet|floated,sealed,drifted,hummed
polar|igloo,sled,husky,iceberg,seal,parka,tundra,aurora|freezing,white,vast,silent|howled,drifted,froze,glittered
savanna|lion,zebra,acacia,herd,plain,watering_hole,dust,sunset|golden,dry,wide,hot|roamed,grazed,roared,stampeded
coral_aquarium|tank,filter,anemone,clownfish,pebble,pump,glass,kelp|bright,calm,glowing,clear|swam,bubbled,hummed,drifted
bee_hive|honey,comb,queen,drone,wax,smoker,veil,frame|golden,sticky,busy,warm|buzzed,swarmed,hummed,dripped
ant_colony|tunnel,queen,soldier,crumb,mound,trail,larva,leaf|tiny,busy,tireless,dark|marched,carried,dug,swarmed
birdwatching|binoculars,hide,feather,nest,wing,song,perch,field_guide|quiet,patient,rare,early|chirped,perched,fluttered,soared
wolf_pack|howl,den,pup,alpha,moon,snow,trail,prey|gray,hungry,wild,fierce|howled,hunted,prowled,circled
bat_cave|wing,guano,roost,echo,dusk,ceiling,swarm,fruit|dark,damp,leathery,high|swooped,squeaked,hung,fluttered
snake|scale,coil,fang,burrow,venom,rattle,skin,rock|slim,silent,cold,slick|hissed,slithered,coiled,shed
butterfly|chrysalis,caterpillar,nectar,wing,petal,net,leaf,cocoon|bright,delicate,tiny,fragile|fluttered,emerged,drifted,landed
sheepdog_trial|crook,whistle,pen,flock,gate,field,collie,course|swift,tense,focused,wide|herded,whistled,circled,dashed
horse_stable|saddle,hay,bridle,hoof,stall,groom,brush,paddock|warm,musty,gentle,strong|neighed,stamped,snorted,trotted
rodeo_ring|lasso,bull,hat,arena,chute,rope,boot,calf|dusty,loud,wild,tough|bucked,roped,roared,spun
carpet_shop|rug,loom,knot,dye,pattern,tassel,wool,bazaar|thick,woven,rich,patterned|unrolled,gleamed,sold,spread
spice_market|cinnamon,pepper,saffron,sack,scoop,scale,clove,stall|fragrant,spicy,warm,rich|wafted,filled,sold,weighed
tea_plantation|leaf,terrace,picker,basket,hill,mist,shrub,factory|green,misty,steep,fragrant|grew,picked,withered,steamed
rice_paddy|seedling,water,farmer,terrace,hat,sickle,dike,buffalo|green,flooded,flat,golden|swayed,grew,glittered,ripened
windmill_farm|blade,tower,tulip,canal,field,sail,dike,gear|tall,slow,flat,wide|turned,creaked,spun,ground
solar_farm|panel,array,inverter,field,cable,fence,sunlight,grid|silent,bright,flat,vast|gleamed,tracked,hummed,tilted
power_plant|turbine,boiler,coil,cable,tower,pylon,control_room,steam|huge,hot,humming,tense|spun,hummed,roared,cooled
dam|spillway,gate,reservoir,concrete,turbine,crest,valley,lock|massive,grey,thick,steady|held,roared,spilled,loomed
canal|lock,barge,towpath,gate,bridge,reed,mule,lockkeeper|slow,narrow,calm,green|drifted,filled,opened,glided
tunnel_construction|drill,shaft,tunnel,helmet,dust,rail,beam,crew|dark,deep,loud,narrow|bored,echoed,rumbled,shook
skyscraper|elevator,lobby,glass,steel,crane,window,roof,floor|tall,sleek,glassy,vast|rose,gleamed,swayed,loomed
subway|platform,turnstile,tunnel,token,map,rail,train,station|crowded,loud,dim,late|screeched,rumbled,rushed,stopped
bus_depot|bus,route,driver,ticket,stop,schedule,fare,shelter|late,crowded,slow,noisy|honked,idled,stopped,rolled
bicycle_shop|chain,tire,pedal,wheel,spoke,bell,helmet,pump|greasy,shiny,quick,light|rang,spun,squeaked,pumped
taxi|meter,fare,cab,driver,street,horn,trunk,window|yellow,late,busy,cramped|honked,idled,stopped,sped
parking_lot|space,meter,ticket,barrier,sign,attendant,row,lamp|full,empty,gray,flat|blinked,filled,emptied,lined
gas_station|pump,nozzle,hose,attendant,canopy,receipt,tank,snack|bright,busy,smelly,open|clicked,hummed,filled,flickered
bank_robbery|mask,vault,alarm,getaway,hostage,safe,guard,duffel|tense,silent,fast,masked|robbed,fled,screamed,triggered
detective|clue,magnifier,trench_coat,suspect,alley,notebook,fingerprint,case|sharp,quiet,grim,cold|sleuthed,questioned,traced,solved
spy|code,disguise,microfilm,safehouse,dossier,gadget,cipher,agent|secret,silent,sly,hidden|slipped,whispered,decoded,vanished
detective_lab|fingerprint,sample,swab,badge,tape,evidence,microscope,gloves|sterile,precise,cold,quiet|dusted,sealed,matched,logged
knight_tournament|lance,horse,helm,banner,herald,joust,shield,crowd|loud,armored,brave,noble|charged,splintered,cheered,galloped
wizard|wand,spell,robe,cauldron,staff,tome,owl,potion|ancient,wise,mystic,strange|chanted,vanished,glowed,brewed
dragon|scale,flame,hoard,cave,claw,wing,gold,knight|fierce,vast,ancient,fiery|roared,soared,breathed,guarded
robot_factory|arm,weld,conveyor,sensor,chip,cell,bolt,line|tireless,precise,metal,quick|whirred,welded,lifted,beeped
submarine_crew|periscope,hatch,sonar,pressure,torpedo,bunk,depth,hull|cramped,silent,deep,tense|dived,pinged,creaked,surfaced
desert_caravan|camel,tent,oasis,dune,saddle,spice,well,veil|slow,dry,tired,golden|trudged,rested,swayed,crossed
arctic_station|antenna,drift,sled,ice_core,dome,generator,parka,storm|remote,freezing,white,silent|howled,drifted,hummed,froze
volunteer_kitchen|ladle,tray,pot,apron,queue,soup,bread,table|warm,generous,busy,plain|served,stirred,queued,ladled
chess_cafe_clock|bell,timer,tray,table,note,napkin,pencil,menu|quiet,tense,warm,small|ticked,rang,waited,stirred
yoga_studio|mat,block,strap,incense,mirror,breath,pose,towel|calm,quiet,slow,soft|stretched,breathed,bent,settled
gym_weights|barbell,plate,rack,mirror,bench,towel,mat,bottle|heavy,sweaty,loud,tough|clanged,lifted,grunted,dropped
swimming_pool|lane,float,goggles,diving_board,ladder,lifeguard,tile,towel|blue,cool,echoing,wet|splashed,rippled,echoed,gleamed
skating_rink|ice,blade,rink,helmet,boot,rail,bench,music|smooth,cold,crowded,bright|glided,scraped,spun,fell
ski_resort|lift,slope,pole,goggles,lodge,snowboard,cabin,run|steep,white,crisp,icy|carved,glided,swooshed,sparkled
surf_shop|board,wax,wetsuit,leash,fin,wave,rack,sandal|salty,sunny,bright,laid_back|dripped,stood,gleamed,rolled
climbing_wall|rope,harness,chalk,hold,carabiner,helmet,route,mat|steep,tough,chalky,high|gripped,climbed,slipped,swung
mountain_rescue|rope,stretcher,helicopter,sled,radio,probe,beacon,tent|urgent,cold,steep,grim|searched,radioed,rescued,hauled
garage_band_room|amp,cable,pedal,cymbal,drum,mic,cord,stool|loud,cramped,hot,dusty|buzzed,crashed,hummed,rattled
kindergarten|crayon,block,nap_mat,cubby,story,finger_paint,apron,song|tiny,noisy,colorful,sticky|giggled,scribbled,napped,sang
"""
_ROWS = [r.split("|") for r in _XL.strip().splitlines()]
# rows too close to an evaluation topic (law, religion, music, medicine, space, ocean, sports, chess/poker, cars, aviation...)
NEAR_EVAL = {"courtroom_clerk", "police_station", "bank_robbery", "detective", "detective_lab", "monastery", "cathedral_tower",
             "temple_garden", "funeral", "opera_house", "garage_band_room", "ballet_school", "hospital_ward", "laboratory",
             "observatory", "space_station_kitchen", "coral_aquarium", "lighthouse_keeper", "shipyard", "submarine_crew",
             "swimming_pool", "skating_rink", "ski_resort", "surf_shop", "climbing_wall", "gym_weights", "mountain_rescue",
             "chess_cafe_clock", "casino_floor", "taxi", "bus_depot", "bicycle_shop", "parking_lot", "gas_station", "pyramid",
             "archaeology", "geology_field", "weather_station", "computer_lab", "robotics_lab", "power_plant"}
XL_TOPICS = {t.replace("_", " "): _make2(t, n.split(","), a.split(","), v.split(",")) for t, n, a, v in _ROWS
             if t not in NEAR_EVAL}
FIT_TOPICS_XL = {**FIT_TOPICS_BIG, **XL_TOPICS}
