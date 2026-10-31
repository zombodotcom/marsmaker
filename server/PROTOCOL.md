# MarsScape protocol, version 1

JSON text frames over one WebSocket. All coordinates are **tiles**: 2 m squares in the viewer's
site-local metres, `tx = floor(x / 2)`, `tz = floor(z / 2)`. The server never sees terrain; the
client does pathfinding and the server only checks that steps are adjacent and inside bounds.

Fields marked *(extra)* are additions beyond the original spec. Clients may ignore them; they
will only ever be added to, never changed.

## HTTP

| Request | Response |
|---|---|
| `GET /health` | `ok` (text) |
| `GET /room/<site_id>` with `Upgrade: websocket` | the room's WebSocket |
| `GET /room/<site_id>~cave` with `Upgrade: websocket` *(extra)* | the lava tube under that site (see [Lava-tube cave](#lava-tube-cave-extra)) |
| `GET /room/<site_id>/info` *(extra)* | `{"site","room","players","max","bounds","bots","ticking","tick","surge","rows_written"}` JSON, CORS `*`. `players` counts humans only; `room` is `"surface"` or `"cave"`; `surge` is the Rift Surge phase; `rows_written` counts SQLite rows written since the room last started. |

`site_id` is the viewer's site name or live id. It must match `[A-Za-z0-9_-]{1,64}`, optionally
followed by `~cave`; anything else gets `400`. One Durable Object (room) exists per site id, and
one more per cave. Any Origin is allowed.

## Session

1. Client opens the socket and sends `hello` within 15 s (otherwise it is closed).
2. Server replies `welcome`, and tells everyone else `join`.
3. Every 600 ms the server sends `tick`. Events arrive between ticks as they happen.
4. On close, everyone else gets `leave`. Progress is saved.

Keep the `token` from `welcome` (e.g. in localStorage) and send it in the next `hello` to get the
same character back. Tokens are per site: each site is its own world. The site's cave uses the same
token and the same character.

Progress is saved when you leave, travel, bank or trade, and otherwise every 50 ticks (30 s) while
something changed.

## Client → server

```json
{"t":"hello","ver":1,"name":"Ada","token":"","bounds":[minTx,minTz,maxTx,maxTz],"spawn":[tx,tz]}
```
- `name`: 1–12 of `[A-Za-z0-9 _-]`; anything else is stripped. Empty or rude names become `Rover<id>`.
- `token`: empty for a new player, or 32 hex chars from an earlier `welcome`.
- `bounds`: the playable tile rectangle (inclusive). Clamped to at most 4096 tiles per side,
  keeping the centre. **The first player's bounds and spawn fix the room's world for its lifetime**;
  later players' values are ignored.
- `spawn`: preferred spawn tile (the landing site). Returning players appear at their last tile.

```json
{"t":"walk","path":[[tx,tz],...],"run":false}
```
At most 256 steps. Each step must be one of the 8 neighbours of the previous step (the first step
may be up to 2 tiles from the server position, the gap is bridged). The whole path is rejected
with `err` if any step is out of bounds or not adjacent. The server moves 1 tile per tick, 2 with
`"run":true`. A new `walk` replaces the old path and cancels the current action.

```json
{"t":"act","node":id}
```
Gather from a node. Send it right after the `walk` that ends next to the node: the action waits
until the path is done. You must end within 1 tile (Chebyshev) of the node. The server then tries
every 4 ticks until the node depletes, the inventory is full, or you walk away.

```json
{"t":"craft","recipe":"brick"}
```
Use the fabricator (node id 0); stand within 1 tile. Keeps making more while materials last.

```json
{"t":"drop","slot":n}        // 0..27, destroys the whole slot (no ground items yet)
{"t":"drop","slot":n,"qty":k|"all"}  // (extra) destroys k off the stack in that slot ("all" = the whole slot)
{"t":"chat","msg":"..."}     // ≤80 chars after cleaning, 1 per second
{"t":"ping"}                 // → {"t":"pong","n":tick}
{"t":"attack","npc":id}      // fight a void creature (see Combat)
{"t":"equip","slot":n}       // wield the weapon in that slot (swaps with the current one)
{"t":"unequip"}              // put the weapon back in the inventory
{"t":"eat","slot":n}         // use a medkit: +20 hp
```

*(extra)* messages, each explained in its own section below:

```json
{"t":"equip","slot":n}                       // also wears armour: it goes to the item's gear slot
{"t":"unequip","gear":"helmet"}              // "weapon" (default), "helmet", "suit", "shield", "cape"
{"t":"act","node":id}                        // also: "locker" opens the bank, "pit" climbs down, "ladder" climbs up
{"t":"bank_deposit","slot":n,"qty":k}        // qty: a positive integer or "all"
{"t":"bank_withdraw","idx":n,"qty":k}        // idx into the bank's items list
{"t":"trade_request","id":playerId}
{"t":"trade_offer","slot":n,"qty":k}         // qty: a positive integer or "all"
{"t":"trade_remove","idx":n,"qty":k}         // idx into your own offer list
{"t":"trade_accept"}
{"t":"trade_decline"}
{"t":"talk","id":playerId}                   // a colonist; you walk over first
{"t":"dialog_choose","option":"accept"}      // an option id from the last dialog
{"t":"rub","slot":n,"skill":"mining"}        // use an Insight Lamp on a skill
{"t":"shop_open","id":traderId}              // the trader colonist (id optional); you walk over first
{"t":"shop_buy","idx":n,"qty":k}             // idx into the shop's stock list; qty a positive integer or "all"
{"t":"shop_sell","slot":n,"qty":k}           // from an inventory slot; qty a positive integer or "all"
{"t":"shop_buyback","idx":n}                 // idx into your buyback list
{"t":"shop_close"}
{"t":"value","item":"medkit"}                // what the trader charges / pays for one, right now
```

Rate limits on top of the general one *(extra)*: `talk` once per 400 ms, `rub` once per 300 ms,
bank actions once per 100 ms, `trade_request` once per 500 ms, `shop_open` once per 300 ms,
shop buy/sell/buyback once per 100 ms and `value` once per 100 ms. A message over its limit is
refused with `{"t":"err","text":"Slow down."}`.

**While you are trading**, anything that takes items out of your inventory is refused with
`{"t":"msg","text":"Finish or decline your trade first."}`. That covers `drop`, `eat`, `craft`,
`equip`/`unequip`, `rub`, the bank, and climbing down the pit.

## Server → client

```json
{"t":"welcome","id":7,"token":"<32 hex>","tick_ms":600,
 "you":{"name":"Ada","tx":0,"tz":0,
        "skills":{"mining":0,"survey":0,"fabrication":0,"ranged":0,"defence":0,"hitpoints":1154},
        "inv":[{"item":"regolith","qty":1},null,...],      // always 28 entries
        "weapon":null,"hp":10,"max_hp":10,"combat":3},
 "players":[{"id","name","tx","tz"}],                    // others already online (not you); colonists add "bot":true,"role" (extra)
 "nodes":[{"id","kind","tx","tz","up":true}],
 "npcs":[{"id","kind","tx","tz","hp","max","level","dead"}],
 "bounds":[...],"spawn":[tx,tz],"tick":1234,             // (extra)
 "xp_table":[0,0,83,174,...],                            // (extra) index = level, xp needed
 "recipes":[{"id","level","xp","inputs":{"regolith":2},"output"}],  // (extra)
 "items":{"regolith":{"name":"Regolith","stack":true},...},         // (extra) stack: see Items
 "weapons":{"none":{"level","speed","acc","str","range"},...},     // (extra)
 "armour":{"iron_suit":{"slot":"suit","level":25,"def":28},...},     // (extra) see Armour
 "quest_defs":[{"id","name","giver","qp","req":{"survey":40},"steps":[{"kind","target","n","text"}],"reward":"text"}],  // (extra)
 "creatures":{"skitter":{"name","level","hp","max_hit","range"},...}, // (extra)
 "room":"surface",                                       // (extra) or "cave"
 "event":null,                                           // (extra) the current Rift Surge event, or null
 "cave":{"w":120,"h":120,"rle":"..."},"exit":[tx,tz]}    // (extra) cave rooms only
```
`you` also carries *(extra)* `"gear":{"helmet","suit","shield","cape"}` (item id or null each),
`"quests":{"<id>":{"s":"active"|"done","step":n,"p":n}}` and `"quest_points":n`.
Recipes may carry *(extra)* `"quest":"<id>"`: that quest must be done before you can make it.
`you.combat` *(extra)* is the combat level. `npcs[].level` and `npcs[].dead` are *(extra)*.
Skill values are XP, not levels. Levels: use `xp_table` or the formula below.

```json
{"t":"tick","n":1235,"p":[[id,tx,tz,anim],...],"m":[[id,tx,tz,anim,hp],...],"full":true}
```
Sent every tick, even when nothing moved. `p` are players and `m` are creatures whose tile, anim (or
hp) changed since the last tick. Every 10th tick has `"full":true` *(extra)* and lists everyone.
The `p` list includes you.

Player anim: `0` idle, `1` walk, `2` run, `3` mine, `4` survey, `5` craft, `6` attack *(extra)*.
Creature anim: `0` idle, `1` walk, `6` attack, `7` dead.

Events:

```json
{"t":"join","id","name","tx","tz"}                // colonists add "bot":true,"role":"guard" (extra)
{"t":"leave","id"}
{"t":"chat","id","name","msg"}
{"t":"node","id","up":false}            // depleted / respawned
{"t":"xp","skill":"mining","xp":1234,"gain":18}
{"t":"level","skill":"mining","level":12}
{"t":"inv","inv":[...28 slots]}
{"t":"msg","text":"You scoop up some regolith."}   // game messages for the chat box
{"t":"err","text":"..."}                           // a request was refused
{"t":"equip","weapon":"pistol"|null}
{"t":"hit","src":"p:7","dst":"n:3","dmg":2}        // dmg 0 = miss; sent to everyone
{"t":"death","who":"n:3"}                          // or "p:7"; sent to everyone
{"t":"hp","hp":8,"max":10}                         // yours
{"t":"heal","src":"p:4","dst":"p:7","amt":5}       // (extra) a medic colonist healed someone; sent to everyone
{"t":"pong","n":1235}
```

*(extra)* events, each explained in its own section below:

```json
{"t":"gear","weapon":"pistol","helmet":"iron_helmet","suit":null,"shield":null,"cape":null,"bonus":14}
{"t":"travel","room":"<site>~cave"}            // reconnect to that room with your token; the server closes this socket (1000 "travel")
{"t":"bank","items":[{"item","qty"}],"slots":100}
{"t":"trade_request","from":id,"name":"Bea"}
{"t":"trade","stage":1,"with":id,"name":"Bea","mine":[{"item","qty"}],"theirs":[...],"accepted":{"me":false,"them":false}}
{"t":"trade_end","result":"done"|"cancelled","reason":"..."}
{"t":"dialog","npc":id,"name":"Rhea","role":"quartermaster","text":"...","quest":"brick_drive"|null,"options":[{"id","text"}]}
{"t":"dialog_end"}
{"t":"quest","id":"brick_drive","state":"active"|"done","step":0,"progress":3,"goal":5,"qp":1}
{"t":"event","kind":"surge","phase":"warning"|"open"|"boss"|"ended","tx","tz","ends_in":200}
{"t":"npc_spawn","npc":{"id","kind","tx","tz","hp","max","level","dead"}}   // a creature that wasn't in welcome
{"t":"npc_despawn","id":100004}
{"t":"shop","npc":id,"name":"Juno","stock":[{"item","qty","price","sell"}],"buyback":[{"item","qty","price","expires_in"}],"sell":{"<item>":price},"credits":n}
{"t":"shop_end","reason":"..."}
{"t":"value","item":"medkit","buy":64|null,"sell":24|null}
```
The weapon-only `equip` event is still sent on every weapon change, for older clients.
After a gather or craft the order is `msg`, `xp`, (`level`), `inv`.

## Skills and levels

Six skills: `mining`, `survey`, `fabrication`, `ranged`, `defence`, `hitpoints`. Max level 99.
XP for level L = `floor( sum_{l=1}^{L-1} floor(l + 300 * 2^(l/7)) / 4 )`: level 2 = 83 xp,
level 10 = 1154, level 99 = 13,034,431. XP caps at 200,000,000.

Hitpoints start at level 10. Max hp = hitpoints level.
Combat level = `floor( 0.25 * (defence + hitpoints) + 0.325 * floor(1.5 * ranged) )`, with levels.
A new player is combat 3.

## Nodes

Placed deterministically from a hash of the site id plus the room's bounds and spawn. About 1 per
400 tiles², between 12 and 600, a quarter of them within ~40 tiles of spawn (with at least one of
each gather kind). Rifts are extra: 3–40, two of them 25–40 tiles from spawn.

| kind | skill | level | xp | item | respawn (ticks) |
|---|---|---|---|---|---|
| `regolith` | mining | 1 | 18 | `regolith` | 8 |
| `ice` | mining | 15 | 35 | `ice` | 20 |
| `hematite` | mining | 30 | 65 | `hematite` | 45 |
| `outcrop` | survey | 1 | 25 | `sample` | 25 |
| `void_vein` *(extra, cave)* | mining | 45 | 90 | `void_crystal` | 60 |
| `fossil` *(extra, cave)* | survey | 40 | 80 | `ancient_relic` | 80 |
| `fabricator` | — | — | — | crafting station, id 0 on the surface, never depletes | — |
| `rift` | — | — | — | a tear into the void where creatures come from, never depletes | — |
| `locker` *(extra)* | — | — | — | the colony locker (bank), 2 tiles from the fabricator | — |
| `pit` *(extra)* | — | — | — | the way down into the lava tube, 10–40 tiles from spawn | — |
| `ladder` *(extra, cave)* | — | — | — | the way back up, id 0 in the cave | — |

The locker and the pit are appended after the rifts (their own random seed), so older node ids
and positions don't move.

Success per attempt: `0.3 + 0.015 * (level - req)`, +0.15 for mining with a `drill_bit` in the
inventory, clamped to 0.05–0.95.

## Recipes (fabrication, at the fabricator)

| recipe | level | inputs | xp |
|---|---|---|---|
| `brick` | 1 | 2 regolith | 12 |
| `water` | 10 | 1 ice | 22 |
| `medkit` | 15 | 1 water + 1 sample | 30 |
| `iron_plate` | 20 | 2 hematite | 45 |
| `pistol` | 25 | 2 iron_plate | 60 |
| `drill_bit` | 35 | 1 sample + 1 iron_plate | 90 |
| `rifle` | 40 | 4 iron_plate + 1 drill_bit | 140 |
| `plasma_gun` | 60 | 3 iron_plate + 5 void_crystal | 300 |
| `chitin_helmet` *(extra)* | 10 | 3 chitin | 30 |
| `chitin_suit` *(extra)* | 15 | 5 chitin | 45 |
| `iron_helmet` *(extra)* | 30 | 2 iron_plate + 1 chitin | 70 |
| `iron_suit` *(extra)* | 35 | 4 iron_plate + 2 chitin | 110 |
| `void_shield` *(extra)* | 50 | 2 iron_plate + 6 void_crystal | 180 |
| `relic_helmet` *(extra, needs quest `echoes`)* | 70 | 1 ancient_relic + 2 iron_plate + 4 void_crystal | 260 |
| `relic_suit` *(extra, needs quest `echoes`)* | 70 | 2 ancient_relic + 3 iron_plate + 6 void_crystal | 400 |

Items: `regolith`, `ice`, `hematite`, `sample`, `brick`, `water`, `iron_plate`, `drill_bit`,
`pistol` (Blaster), `rifle` (Pulse Rifle), `plasma_gun` (Plasma Cannon), `medkit`, `chitin` (Xeno Chitin),
`void_crystal` (Void Crystal, stacks), and *(extra)* `ancient_relic` (Ancient Relic), `xp_lamp`
(Insight Lamp), `chitin_helmet`, `chitin_suit`, `iron_helmet`, `iron_suit`, `void_shield`,
`relic_helmet` (Relic Helm), `relic_suit` (Relic Armour), `rift_blade` (Rift Blade) and
`void_cape` (Void Cape).

**Stacking** *(extra)*: every item stacks in a single inventory slot (`{"item","qty"}` with any
`qty`) except equippable gear, which takes one slot per item: the weapons `pistol`, `rifle`,
`plasma_gun`, `rift_blade` and the armour `chitin_helmet`, `chitin_suit`, `iron_helmet`, `iron_suit`,
`void_shield`, `relic_helmet`, `relic_suit`, `void_cape` (`GEAR_ITEMS` in game.ts; the viewer's
proto.rs mirrors it). Anything gained (gathering, crafting, loot, quest rewards, the shop, the
locker, a trade) joins the existing stack, so a full pack still takes more of an item you already
hold; only a new kind needs a free slot. Crafting takes inputs from stacks and is refused (nothing
consumed) if its output wouldn't fit. `eat` and `rub` use one from the stack. Saves from before
stacking (one item per slot) are merged into stacks when loaded.

## Combat

Lore: ancient Martians sealed something beneath the planet; the rifts are tearing open into the
void and space creatures pour out. Item ids `demon_core` / `bones` from before this re-skin are
read as `void_crystal` / `chitin` when a saved player loads.

| weapon | ranged level to equip | attack every (ticks) | range (tiles) |
|---|---|---|---|
| none (punch) | — | 4 | 1 |
| `pistol` | 1 | 4 | 7 |
| `rifle` | 20 | 5 | 7 |
| `plasma_gun` | 40 | 6 | 7 |
| `rift_blade` *(extra, Rift Surge unique)* | 50 | 4 | 2 |

`attack` sets your target. If it is out of range the server steps you toward it each tick (you
can also `walk` first). Line of sight is not checked (the server has no terrain).

- Your accuracy roll is `(ranged + 8) * (weapon acc + 64)`. The creature's defence roll is `(def + 9) * 64`.
- Hit chance is `A > D ? 1 - (D+2)/(2(A+1)) : A/(2(D+1))`.
- A creature's attack roll is `(att + 9) * 64`. Your defence roll is `(defence + 9) * (armour bonus + 64)` *(extra: the armour bonus; it was 0 before)*.
- Max hit is `floor(0.5 + (ranged + 8) * (weapon str + 64) / 640)`. Damage is uniform from 0 to the max hit.
- XP per damage dealt: ranged ×4, hitpoints ×1.33, and defence ×1 *(extra, so defence trains)*.

| creature | level | hp | max hit | attacks every | drops |
|---|---|---|---|---|---|
| `skitter` (Void Skitter) | 5 | 5 | 1 | 4 | chitin, crystal 20%, blaster 1/25 |
| `stalker` (Dust Stalker) | 20 | 30 | 5 | 4 | chitin, crystal 50%, blaster 1/15, pulse rifle 1/80 |
| `golem` (Rift Golem) | 45 | 70 | 10 | 6 | chitin, 1 crystal, pulse rifle 1/30 |
| `leviathan` (Void Leviathan, boss) | 90 | 250 | 20 (range 4) | 5 | chitin, 5 crystals, plasma cannon 1/4 |
| `glowworm` *(extra, cave)* | 30 | 40 | 6 | 4 | chitin, crystal 40%, pulse rifle 1/40 |
| `crystal_horror` *(extra, cave)* | 60 | 90 | 13 (range 2) | 5 | chitin, 2 crystals, ancient relic 1/6, plasma cannon 1/40 |

**Creature AI:**
- **Placement:** creatures live at rifts. The two rifts near spawn hold skitters, and the farthest rift also holds one leviathan.
- **Wandering:** an idle creature wanders within 4 tiles of home.
- **Aggression:** a creature attacks the nearest player within 5 tiles whose combat level is below 2× its own.
  - Combat is single-way: only one creature goes after a given player at a time.
  - A creature you attack fights back, unless it is already fighting someone else.
- **Chasing:** a creature chases up to 15 tiles from home, then walks back and heals.
- **Death and loot:** a dead creature respawns after 50 ticks. Its loot goes straight into the killer's inventory, and anything that doesn't fit is lost.

**Player death:** you respawn at spawn with full hp and lose the equipped weapon. Hp regenerates
1 per 10 ticks. A medkit restores 20 hp and delays your next shot by 2 ticks. There is no PvP.

## Colonists *(extra)*

Every room has a few server-run NPC colonists, so a site never feels empty and new players get
help. To a client they are ordinary players: they appear in `welcome.players` and `join`, move in
`tick` rows with the normal anims, chat with `chat`, and show up in `hit` / `death` / `heal` as
`p:<id>`. Their player entries carry `"bot":true` and a `"role"`, which is useful for name tags
such as "Vega (Guard)".

| role | what it does |
|---|---|
| `quartermaster` | Stays by the fabricator. Gives each new character a one-time starter kit (3 bricks + 1 medkit + 100 credits) when they come within 4 tiles. The kit is tracked per token and survives restarts. |
| `guard` | Patrols spawn and lookouts near the nearest rifts. Attacks any creature that jumps a player within 8 tiles. It leaves alone a creature the player is fighting on purpose, so it never steals a kill. |
| `medic` | Walks to anyone below 50% hp within 40 tiles and heals +5 hp, at most once every 8 ticks per medic. |
| `trader` | Minds the stall next to the fabricator and runs the shop (see Trader and credits). Always present, not counted in the population below. |
| `miner` / `surveyor` | Gathers near spawn, away from rifts. Miners craft at the fabricator. Both "store" their haul in the colony depot, which means it is simply cleared. |

**Population:** about 4 colonists with one human, one fewer per extra human, and never fewer than 1.
- The count is `clamp(5 - humans, 1, 4)`.
- Roles go in this order: quartermaster, guard, medic, then a miner or a surveyor (fixed per site).
- Names are stable per site.
- Colonists leave with a `leave` and come back with a `join`.

**Chat commands**, said in public chat:
- **`help` / `follow me`:** the nearest free guard (or else the nearest free colonist) answers. It follows you for 2 minutes and fights anything that attacks you.
- **`stop` / `bye` / `dismiss`:** sends your escort back to its post.
- **Greetings:** colonists answer hi/hello now and then. They greet new players with a tip and drop the odd line of lore. They speak at most one line every 4 ticks per room.

Colonists follow the same rules as players. They have levels for their role, use the same gather,
craft and combat formulas, can be attacked, die, and respawn at spawn. They are never saved. The
room still stops ticking and hibernates when the last *human* leaves.

**Dev only:** with `MARSSCAPE_TEST=1` (`wrangler dev --var MARSSCAPE_TEST:1`), `{"t":"test",...}`
accepts `hp` (set your hp), `skills` (`{"ranged":xp,...}`), `give` (a list of item ids),
`credits` (a number: adds that many credits), `surge` (`"now"` starts a 3-tick warning, `"boss"` brings the boss next to you) and
`defsim` + `n` (samples a creature kind's hit rate against you; answered with
`{"t":"test","hit_rate","bonus"}`). In production that message is answered with "Unknown message type".

## Lava-tube cave *(extra)*

Every site has a lava tube under it: its own room, `<site_id>~cave`.
- **Going down:** `act` on the surface `pit` (walk next to it first). The server saves you and sends
  `{"t":"travel","room":"<site_id>~cave"}`, then closes the socket with `1000 "travel"`. Connect to
  `/room/<site_id>~cave` and send the usual `hello` with your token (`bounds` and `spawn` are ignored
  in the cave). A cave `hello` without a known token is refused: you can only arrive through the pit.
- **Your character comes with you:** skills, inventory, gear, hp and quests. The cave borrows it from
  the surface room and hands it back when you leave, so nothing can be in two places.
- **Coming back up:** `act` on the `ladder` (node id 0, at `exit`) sends `travel` back to `<site_id>`,
  and you reappear next to the pit. Dying in the cave sends you up to the landing site (you lose
  your weapon, as on the surface). Disconnecting in the cave also puts you next to the pit next time.
- **Logging in on the surface while still in the cave** (another tab, a dead connection) takes the
  character back from the cave; the cave socket gets `err` "Logged in from another location.".
- **No colonists, fabricator, bank, trading or Rift Surges** down there.

**Layout.** 120 × 120 tiles, `0 <= tx < w`, `0 <= tz < h`, generated deterministically from the
surface site id: a meandering tube with side branches and chambers. `welcome.cave.rle` is the map as
comma-separated run lengths over the tiles in row-major order (`index = tz * w + tx`), alternating
wall, floor, wall, ... and **starting with a wall run** (which may be 0). The runs add up to `w * h`.

```js
const g = new Uint8Array(w * h); let i = 0, v = 0;           // 0 = wall, 1 = floor
for (const n of rle.split(",").map(Number)) { g.fill(v, i, i + n); i += n; v ^= 1; }
```

**Walls are enforced.** Every step of a `walk` must land on floor, and a diagonal step also needs
both orthogonal neighbours to be floor (no corner cutting). Otherwise the whole path is refused
with `err` "You can't walk there.". Creatures follow the same rule. Line of sight is still not checked.

**Content.** About 12 `void_vein` (mining 45 → void crystals) and 8 `fossil` (survey 40 → ancient
relics) in the far chambers; 14 `glowworm` along the tubes and a few `crystal_horror` in the far
chambers.

## Trader and credits *(extra)*

**Credits** (`credits`) are an ordinary stackable item: they sit in your pack, stack in the locker and
can be traded. You get them from:
- the quartermaster's starter kit (+100 credits, new characters only);
- every creature kill: about its level, randomised (`floor(level * (0.5..1.5))`, at least 1);
- some quests (Pest Control 60, Dust to Dust 200, Heavy Metal 400);
- selling to the trader.

**The trader** is a colonist with `"role":"trader"`. She is present in every surface room (on top of
the usual crew, so she is never scaled away) and stands one tile south (+z) of the `stall` node,
which sits 3 tiles from the fabricator on the side away from the locker. `act` on the stall or
`shop_open` both walk you to within 2 tiles of her and open the shop. She never trades with
`trade_request`, never escorts, and only fights back if attacked.

**Opening:** you get `shop`:
- `stock`: the shelf, shared by everyone in the room. `price` is what one unit costs right now
  (0 when sold out), `sell` what she'd pay for one.
- `buyback`: your own recent sales (newest first, at most 10). `price` is the total you were paid,
  which is exactly what buying it back costs. `expires_in` in seconds.
- `sell`: what she'd pay for one of each item in your pack right now (0 = she won't take it).
- `credits`: how many credits you carry.

`shop` is sent again after each of your transactions, when anyone else changes the shelf while your
window is open, and on every restock. `shop_end` closes the window: you walked more than 2 tiles
away, started a trade, or sent `shop_close`.

**Prices.** Every item has a base value (`VALUE` in game.ts). With `n` = the item's normal stock and
`q` = the units on the shelf, `f = clamp(q / 2n, 0, 1)`:
- buy one: `ceil(value * (1.6 - 0.6 f))`: 1.6x when nearly sold out, 1.0x when the shelf is overflowing;
- sell one: `floor(value * (0.6 - 0.3 f))`: 0.6x when she has none, 0.3x when overflowing.
  For an item she doesn't normally stock, `f = min(1, q / 10)`.

Because the best sell price (0.6x) is below the lowest buy price (1.0x), buying and selling back can
never make a profit. Multi-unit buys and sells are priced one unit at a time, so a big sale slowly
lowers its own price. `shop_buy` buys as many as you can afford and fit.

**Normal stock:** medkit 10, water flask 15, regolith brick 25, iron plate 10, chitin helmet 3,
blaster 3, Insight Lamp 2. Everything you sell joins the shelf (at most 500 of an item). Every
30 s (50 ticks) each shelf moves one unit back toward its normal stock, so items she doesn't
normally stock dwindle and vanish.

**Won't buy:** the Rift Surge trophies (`rift_blade`, `void_cape`, bound to whoever earned them)
and credits themselves.

**Buyback.** Each sale adds one entry. Buying it back costs exactly what you were paid and takes
those goods back off the shelf (as many as are still there). The list is per character and per
session: it is cleared when you log out or leave the room (e.g. climbing into the lava tube), and
entries expire after 30 minutes.

**Rules.** All checks happen on the server: you must be within 2 tiles of the trader, not trading,
with enough credits and inventory space. Each transaction is computed on a copy and committed in
one step together with your save, so nothing is duplicated or lost.

## Colony locker (bank) *(extra)*

The `locker` stands two tiles from the fabricator. `act` on it (walk next to it first) opens your
bank: `{"t":"bank","items":[{"item","qty"},...],"slots":100}`. Every item kind stacks in the bank,
up to 100 different kinds. The bank is per character (token) and per site, and is saved at once.

- `bank_deposit` puts `qty` of the item in inventory slot `slot` into the bank, taking from every
  slot holding that item (`"all"` = all of them).
- `bank_withdraw` takes `qty` of bank entry `idx` into the inventory. Stackable items arrive as one
  stack; gear needs one free slot each, and you get as many as fit. When an entry reaches 0 it is removed and later
  indices shift down.
- Both need you within 1 tile of the locker and answer with `inv` and the new `bank`.

## Trading *(extra)*

Between two human players in the same surface room, at most 10 tiles apart.
1. `trade_request` with the other player's id. They get `trade_request`. When they request you
   back (within 30 s) the trade opens: both get `trade` with `stage` 1.
2. Each side adds items with `trade_offer` (from an inventory slot, up to what you haven't offered
   yet) and takes them back with `trade_remove`. Items stay in your inventory until the end.
   Every change resets both accepts.
3. Both `trade_accept` → `stage` 2, the confirm screen: offers are locked (`err`) and the accepts reset.
4. Both `trade_accept` again → the swap happens at once for both, or not at all. Both get `inv`
   and `trade_end` with `result: "done"`.

`trade_end` with `result: "cancelled"` and a `reason` comes when either side declines, a player
leaves or moves more than 10 tiles away, or the swap is impossible (someone no longer has the items,
or lacks inventory space). Colonists don't trade. Completed trades are logged on the server.

## Quests *(extra)*

Colonists hand out quests. `talk` to a colonist: you walk to within 2 tiles, then get a `dialog`.
Answer with `dialog_choose` and one of its option ids:

| option id | meaning |
|---|---|
| `accept` / `decline` | take the offered quest (`dialog.quest`) or not |
| `offer:<quest id>` | the colonist has several open quests and lists them; pick one to hear its offer |
| `turn_in` | hand in a finished quest and get the reward |
| `bye` | close |

Every choice is answered with `dialog_end` (or a new `dialog` for `offer:`). An option that wasn't
in the last dialog is refused with `err` "That option isn't open.".

A quest has one or more steps. Step kinds: `kill` (creature kind), `gather` (item from a node),
`craft` (recipe output), `deliver` (have the items when you turn in; they are taken), `enter`
(`cave`). Progress comes as `quest` events. Each quest has a giver role; when that role isn't in
the room, the quartermaster gives it instead.

| id | name | giver | needs | steps | reward | QP |
|---|---|---|---|---|---|---|
| `skitter_cull` | Pest Control | guard | — | kill 5 skitter | lamp, medkit | 1 |
| `brick_drive` | Building Blocks | quartermaster | — | craft 5 brick | lamp, medkit | 1 |
| `ice_run` | Thirsty Colony | miner | mining 15 | gather 5 ice | lamp, drill bit | 1 |
| `core_samples` | Rock Solid | surveyor | — | deliver 3 sample | lamp, medkit | 1 |
| `field_hospital` | Field Hospital | medic | fabrication 10 | deliver 2 water | lamp, 2 medkits | 1 |
| `dust_to_dust` | Dust to Dust | guard | ranged 15 | kill 3 stalker | 2 lamps, chitin suit | 2 |
| `heavy_metal` | Heavy Metal | guard | ranged 35 | kill 1 golem | 2 lamps, 5 void crystals | 2 |
| `echoes` | Echoes Below | quartermaster | survey 40 | enter the cave → gather 1 ancient_relic → deliver 1 ancient_relic | 3 lamps, relic armour recipes | 3 |

**Insight Lamp** (`xp_lamp`): `rub` it on a skill for `100 + 25 × level` XP in that skill.

## Armour *(extra)*

Gear slots: `helmet`, `suit`, `shield` (an energy shield generator) and `cape`, beside the weapon.
`equip` an armour item to wear it (it swaps with what was in that slot); `unequip` with `gear`
takes it off. Changes come as `gear` events, whose `bonus` is the total armour bonus.

| item | slot | defence level | bonus |
|---|---|---|---|
| `chitin_helmet` | helmet | 1 | 6 |
| `chitin_suit` | suit | 5 | 12 |
| `iron_helmet` | helmet | 20 | 14 |
| `iron_suit` | suit | 25 | 28 |
| `void_shield` | shield | 40 | 25 |
| `relic_helmet` | helmet | 60 | 24 |
| `relic_suit` | suit | 60 | 48 |
| `void_cape` | cape | 1 | 2 (a Rift Surge trophy) |

The bonus goes into your defence roll (see Combat). Dying loses only the weapon; armour is kept.

## Rift Surge *(extra)*

While humans are on a surface site, a Rift Surge happens every 20–30 minutes of play:
1. `event` with `phase: "warning"`, the spot (`tx`,`tz`, 15–35 tiles from spawn) and `ends_in`
   (ticks; 200 = 2 minutes).
2. `phase: "open"`: three waves of creatures, 20 ticks apart (skitters, then stalkers, then a golem).
   They arrive with `npc_spawn` and attack anyone nearby, with no combat-level or one-at-a-time
   limits. A few ticks after they die they are removed with `npc_despawn` instead of respawning.
3. `phase: "boss"`: a Void Leviathan comes through. Colonist guards within 40 tiles join the fight.
4. `phase: "ended"`. If the boss died, everyone who did at least 5% of its hp gets a loot roll:
   3–8 void crystals, a 1/8 chance of the `rift_blade` and a 1/5 chance of the `void_cape`. The
   boss gives no normal drop. If nobody kills it within 6 minutes, the rift collapses and takes the
   creatures with it.

`welcome.event` carries the current phase for players who arrive mid-surge. When the last human
leaves, the surge is cleared, so it never keeps an empty room awake.

## Limits

- **Message limits:**
  - More than 20 messages a second, any frame over 4 KB, binary frames, or 6 malformed messages close the socket.
  - Chat is capped at 1 per second and is filtered.
- **Players per room:** at most 100 (`err` "This site is full", then close).
- **Idle logout:** 20 minutes without input logs you out. Pings don't count as input.
- **Duplicate login:** logging in with a token that is already online kicks the older socket.
- **Close codes:**
  - `1000`: normal, idle logout, or logged in elsewhere.
  - `1002`: bad hello or version.
  - `1003`: binary frame.
  - `1008`: abuse.
  - `1009`: frame too large.
  - `1013`: room full.
