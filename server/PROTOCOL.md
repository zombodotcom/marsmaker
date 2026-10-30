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
| `GET /room/<site_id>/info` *(extra)* | `{"site","players","max","bounds","bots","ticking","tick"}` JSON, CORS `*`. `players` counts humans only. |

`site_id` is the viewer's site name or live id. It must match `[A-Za-z0-9_-]{1,64}`; anything
else gets `400`. One Durable Object (room) exists per site id. Any Origin is allowed.

## Session

1. Client opens the socket and sends `hello` within 15 s (otherwise it is closed).
2. Server replies `welcome`, and tells everyone else `join`.
3. Every 600 ms the server sends `tick`. Events arrive between ticks as they happen.
4. On close, everyone else gets `leave`. Progress is saved.

Keep the `token` from `welcome` (e.g. in localStorage) and send it in the next `hello` to get the
same character back. Tokens are per site: each site is its own world.

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
{"t":"drop","slot":n}        // 0..27, destroys the item (no ground items yet)
{"t":"chat","msg":"..."}     // ≤80 chars after cleaning, 1 per second
{"t":"ping"}                 // → {"t":"pong","n":tick}
{"t":"attack","npc":id}      // fight a void creature (see Combat)
{"t":"equip","slot":n}       // wield the weapon in that slot (swaps with the current one)
{"t":"unequip"}              // put the weapon back in the inventory
{"t":"eat","slot":n}         // use a medkit: +20 hp
```

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
 "items":{"regolith":{"name":"Regolith","stack":false},...},        // (extra)
 "weapons":{"none":{"level","speed","acc","str","range"},...}}      // (extra)
```
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
| `fabricator` | — | — | — | crafting station, id 0, never depletes | — |
| `rift` | — | — | — | a tear into the void where creatures come from, never depletes | — |

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

Items: `regolith`, `ice`, `hematite`, `sample`, `brick`, `water`, `iron_plate`, `drill_bit`,
`pistol` (Blaster), `rifle` (Pulse Rifle), `plasma_gun` (Plasma Cannon), `medkit`, `chitin` (Xeno Chitin),
`void_crystal` (Void Crystal, stacks). Everything else takes
one slot per item.

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

`attack` sets your target. If it is out of range the server steps you toward it each tick (you
can also `walk` first). Line of sight is not checked (the server has no terrain).

- Your accuracy roll is `(ranged + 8) * (weapon acc + 64)`. The creature's defence roll is `(def + 9) * 64`.
- Hit chance is `A > D ? 1 - (D+2)/(2(A+1)) : A/(2(D+1))`.
- Max hit is `floor(0.5 + (ranged + 8) * (weapon str + 64) / 640)`. Damage is uniform from 0 to the max hit.
- XP per damage dealt: ranged ×4, hitpoints ×1.33, and defence ×1 *(extra, so defence trains)*.

| creature | level | hp | max hit | attacks every | drops |
|---|---|---|---|---|---|
| `skitter` (Void Skitter) | 5 | 5 | 1 | 4 | chitin, crystal 20%, blaster 1/25 |
| `stalker` (Dust Stalker) | 20 | 30 | 5 | 4 | chitin, crystal 50%, blaster 1/15, pulse rifle 1/80 |
| `golem` (Rift Golem) | 45 | 70 | 10 | 6 | chitin, 1 crystal, pulse rifle 1/30 |
| `leviathan` (Void Leviathan, boss) | 90 | 250 | 20 (range 4) | 5 | chitin, 5 crystals, plasma cannon 1/4 |

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
| `quartermaster` | Stays by the fabricator. Gives each new character a one-time starter kit (3 bricks + 1 medkit) when they come within 4 tiles. The kit is tracked per token and survives restarts. |
| `guard` | Patrols spawn and lookouts near the nearest rifts. Attacks any creature that jumps a player within 8 tiles. It leaves alone a creature the player is fighting on purpose, so it never steals a kill. |
| `medic` | Walks to anyone below 50% hp within 40 tiles and heals +5 hp, at most once every 8 ticks per medic. |
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

**Dev only:** with `MARSSCAPE_TEST=1` (`wrangler dev --var MARSSCAPE_TEST:1`), `{"t":"test","hp":n}`
sets your own hp. In production that message is answered with "Unknown message type".

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
