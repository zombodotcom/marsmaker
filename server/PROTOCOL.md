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
| `GET /room/<site_id>/info` *(extra)* | `{"site","players","max","bounds"}` JSON, CORS `*` |

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
{"t":"attack","npc":id}      // fight a demon (see Combat)
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
 "players":[{"id","name","tx","tz"}],                    // others already online (not you)
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
Sent every tick, even when nothing moved. `p` are players and `m` are demons whose tile, anim (or
hp) changed since the last tick. Every 10th tick has `"full":true` *(extra)* and lists everyone.
The `p` list includes you.

Player anim: `0` idle, `1` walk, `2` run, `3` mine, `4` survey, `5` craft, `6` attack *(extra)*.
Demon anim: `0` idle, `1` walk, `6` attack, `7` dead.

Events:

```json
{"t":"join","id","name","tx","tz"}
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
| `rift` | — | — | — | where demons live, never depletes | — |

Success per attempt: `0.3 + 0.015 * (level - req)`, +0.15 for mining with a `drill_bit` in the
inventory, clamped to 0.05–0.95.

## Recipes (fabrication, at the fabricator)

| recipe | level | inputs | xp |
|---|---|---|---|
| `brick` | 1 | 2 regolith | 12 |
| `water` | 10 | 1 ice | 22 |
| `medkit` | 15 | 1 water + 1 sample | 30 |
| `pistol` | 20 | 2 iron_plate | 60 |
| `iron_plate` | 25 | 2 hematite | 45 |
| `drill_bit` | 35 | 1 sample + 1 iron_plate | 90 |
| `rifle` | 40 | 4 iron_plate + 1 drill_bit | 140 |
| `plasma_gun` | 60 | 3 iron_plate + 5 demon_core | 300 |

Items: `regolith`, `ice`, `hematite`, `sample`, `brick`, `water`, `iron_plate`, `drill_bit`,
`pistol`, `rifle`, `plasma_gun`, `medkit`, `bones`, `demon_core` (stacks). Everything else takes
one slot per item.

## Combat

| weapon | ranged level to equip | attack every (ticks) | range (tiles) |
|---|---|---|---|
| none (punch) | — | 4 | 1 |
| `pistol` | 1 | 4 | 7 |
| `rifle` | 20 | 5 | 7 |
| `plasma_gun` | 40 | 6 | 7 |

`attack` sets your target. If it is out of range the server steps you toward it each tick (you
can also `walk` first). Line of sight is not checked (the server has no terrain).

- Your accuracy roll is `(ranged + 8) * (weapon acc + 64)`. The demon's defence roll is `(def + 9) * 64`.
- Hit chance is `A > D ? 1 - (D+2)/(2(A+1)) : A/(2(D+1))`.
- Max hit is `floor(0.5 + (ranged + 8) * (weapon str + 64) / 640)`. Damage is uniform from 0 to the max hit.
- XP per damage dealt: ranged ×4, hitpoints ×1.33, and defence ×1 *(extra, so defence trains)*.

| demon | level | hp | max hit | attacks every | drops |
|---|---|---|---|---|---|
| `imp` | 5 | 5 | 1 | 4 | bones, core 20%, pistol 1/25 |
| `hellhound` | 20 | 30 | 5 | 4 | bones, core 50%, pistol 1/15, rifle 1/80 |
| `brute` | 45 | 70 | 10 | 6 | bones, 1 core, rifle 1/30 |
| `rift_lord` | 90 | 250 | 20 (range 4) | 5 | bones, 5 cores, plasma gun 1/4 |

**Demon AI:**
- **Placement:** demons live at rifts. The two rifts near spawn hold imps, and the farthest rift also holds one rift lord.
- **Wandering:** an idle demon wanders within 4 tiles of home.
- **Aggression:** a demon attacks the nearest player within 5 tiles whose combat level is below 2× its own.
  - Combat is single-way: only one demon goes after a given player at a time.
  - A demon you attack fights back, unless it is already fighting someone else.
- **Chasing:** a demon chases up to 15 tiles from home, then walks back and heals.
- **Death and loot:** a dead demon respawns after 50 ticks. Its loot goes straight into the killer's inventory, and anything that doesn't fit is lost.

**Player death:** you respawn at spawn with full hp and lose the equipped weapon. Hp regenerates
1 per 10 ticks. A medkit restores 20 hp and delays your next shot by 2 ticks. There is no PvP.

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
