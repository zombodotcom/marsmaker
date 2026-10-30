# MarsScape server

MarsScape is the old-school-MMO mode of MarsMaker. It has:
- 600 ms ticks and tile movement;
- six skills: mining, survey, fabrication, ranged, defence and hitpoints;
- a 28-slot inventory, public chat, and void creatures from the rifts to shoot;
- real HiRISE terrain, with one shared world per Mars site.

This folder is the server: a Cloudflare Worker with one Durable Object ("room") per site. Player
progress is stored in the room's SQLite storage. Everything runs on the Cloudflare free plan.
The wire protocol is in [PROTOCOL.md](PROTOCOL.md).

- `src/index.ts`: Worker routing plus the `Room` Durable Object (tick loop, validation, persistence).
- `src/game.ts`: game rules (XP curve, nodes, recipes, weapons, creatures, world generation).
- `src/bots.ts`: the colonists, server-run NPC players that live in every room (see below).
- `test/bots.mjs`: bot test. Bots chat, walk, mine, craft, drop, fight a skitter, and reconnect.
- `test/community.mjs`: colonist test. Covers the greeting, the one-time starter kit, "help" escorts, the medic, scaling, and hibernation.
- `test/load.mjs`: load test. Autonomous clients over several rooms, reporting tick jitter, message rates, errors and CPU.

## Colonists

Each room keeps a few NPC colonists, so a site never feels empty:
- a **quartermaster** at the fabricator, who hands each new character a starter kit;
- a **guard**, who protects players from creatures that jump them;
- a **medic**, who heals anyone below half hp;
- a **miner** or **surveyor**, who works the nodes near spawn.

Population: there are about 4 with one human, one fewer per extra human, and never fewer than 1.

Chat commands:
- **`help`** (or `follow me`): a guard comes and escorts you for 2 minutes.
- **`stop`** (or `bye`): sends it back to its post.

Colonists use the same rules as players and are never saved. A room still stops ticking when its
last human leaves. The details are in [PROTOCOL.md](PROTOCOL.md#colonists-extra).

## Run locally

```sh
cd server
npm install
npx wrangler dev --port 8787 --var MARSSCAPE_TEST:1   # serves http://127.0.0.1:8787, ws://127.0.0.1:8787/room/<site>
node test/bots.mjs                  # in a second terminal; prints PASS/FAIL, takes about 1 minute
node test/community.mjs             # colonists; about 1-2 minutes; the medic check needs MARSSCAPE_TEST
node test/load.mjs ws://127.0.0.1:8787 30 3 300   # 30 clients over 3 rooms for 5 minutes
```

`MARSSCAPE_TEST=1` only enables a dev message that sets your own hp, which the medic check uses.
Never set it in `wrangler.toml`.

No Cloudflare account is needed for local runs. Local room data lives in `server/.wrangler/`; delete
it to reset every world.

## Deploy (free)

1. Create a free account at https://dash.cloudflare.com/sign-up, if you don't have one.
2. In this folder:
   ```sh
   npx wrangler login       # opens the browser once to authorize
   npx wrangler deploy
   ```
3. Wrangler prints the URL, for example `https://marsscape.<your-subdomain>.workers.dev`. Check it:
   `curl https://marsscape.<your-subdomain>.workers.dev/health` should print `ok`.

Redeploying keeps all player data. The `[[migrations]]` block in `wrangler.toml` creates the SQLite
Durable Object class on the first deploy. Leave it as it is, and add a new tagged block only when
adding or renaming classes.

## Point the game at it

The client connects to `wss://<worker-host>/room/<site_id>`, where `<worker-host>` is the URL from
the deploy step with `https://` replaced by `wss://`. During local development use
`ws://127.0.0.1:8787`. How the viewer is told which server to use (a URL parameter or a command-line
flag) is documented in `viewer/README.md` and `viewer/WEB.md`.

## Limits

- 100 players per site.
- Each site is its own world, so characters don't carry between sites.
- The server trusts the client's pathfinding over the terrain and only checks tile adjacency and bounds.
- Free-plan Durable Object usage limits apply. A room only runs its tick loop while someone is
  connected, then hibernates. Colonists don't change that.

## Load test and free-plan budget

**Measured on 2026-10-29**, local `wrangler dev`, with `test/load.mjs`: 30 clients over 3 rooms for 5 minutes, all walking, gathering, crafting, fighting and chatting.

| Measure | Result |
|---|---|
| Tick interval (all rooms) | p50 606 ms, p99 667 ms; 1 interval over 900 ms out of 14,892 |
| Server → clients, per room | about 34 msg/s (6 KiB/s) |
| Clients → server, per room | about 7 msg/s (0.7 msg/s per client) |
| Unexpected disconnects | 0 |
| workerd CPU, 10 humans + 1 colonist per room | 6.7% of one core, 13.5 ms per room-tick including socket I/O |
| workerd CPU, 1 human + 4 colonists per room | 1.4 ms per room-tick |
| SQLite writes | about 1 row per player save (every 10 ticks while a player changes) and 1 per room meta write (every 10 ticks) |

The 10 errors in the run were the load clients sending paths from a stale position ("Path steps must be adjacent tiles.").

**Free plan** (Durable Objects: 100k requests/day, 13,000 GB-s/day, 100k SQLite rows written/day; resets 00:00 UTC; over a limit, those operations fail until the reset). Worked example: **20 humans, 4 hours a day**.

| Limit | What uses it | 20 humans × 4 h/day |
|---|---|---|
| **Duration** (the binding limit) | A room is billed 128 MB for every second it ticks. 13,000 GB-s ≈ **29 room-hours a day**. | 1 room = 1,800 GB-s (14%); 5 rooms = 9,000 (69%); 20 solo rooms = 36,000 (**277%**, they stop after about 1.4 h) |
| **Rows written** | About 600/h per active player plus 600/h per ticking room | 48k + 2.4k (one room) ≈ 50% … about 96% with 20 solo rooms |
| **Requests** | Each WebSocket connect = 1. Incoming messages are billed 20:1. | 20 × 14,400 s × 0.7 msg/s ÷ 20 ≈ 10k/day (10%) |
| Rows read, storage | Logins and gift checks only; tiny | — |

**Risks:**
- **Many solo rooms:** players spread over lots of sites are what blows the free plan, through duration first and then rows written.
- **Cheap fix for rows written:** save players every ~50 ticks instead of 10, and write the room's tick only when it stops. That cuts rows written by about 5×. Rooms already save on disconnect.
