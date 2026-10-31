# MarsScape server

MarsScape is the old-school-MMO mode of MarsMaker. It has:
- 600 ms ticks and tile movement;
- six skills: mining, survey, fabrication, ranged, defence and hitpoints;
- a 28-slot inventory, public chat, and void creatures from the rifts to shoot;
- real HiRISE terrain, with one shared world per Mars site;
- a lava-tube cave under every site, a bank, player trading, a trader's shop with credits,
  colonist quests, armour, and Rift Surge boss events.

This folder is the server: a Cloudflare Worker with one Durable Object ("room") per site. Player
progress is stored in the room's SQLite storage. Everything runs on the Cloudflare free plan.
The wire protocol is in [PROTOCOL.md](PROTOCOL.md).

- `src/index.ts`: Worker routing plus the `Room` Durable Object (tick loop, validation, persistence).
- `src/game.ts`: game rules (XP curve, nodes, recipes, weapons, creatures, world generation).
- `src/bots.ts`: the colonists, server-run NPC players that live in every room (see below).
- `src/cave.ts`: the lava tube under each site (layout generator, wall checks, map encoding).
- `src/bank.ts`: the colony locker (bank) and player trading.
- `src/shop.ts`: the trader's shop (credits, buy / sell / buyback, the shared shelf).
- `src/quests.ts`: colonist quests, dialogs and Insight Lamps.
- `src/events.ts`: the Rift Surge event.
- `test/bots.mjs`: bot test. Bots chat, walk, mine, craft, drop, fight a skitter, and reconnect.
- `test/community.mjs`: colonist test. Covers the greeting, the one-time starter kit, "help" escorts, the medic, scaling, and hibernation.
- `test/features.mjs`: cave, bank, trading, quests, armour and Rift Surge test (needs `MARSSCAPE_TEST`).
- `test/shop.mjs`: shop test: credits, buy / sell / buyback, the price curve, a randomised no-profit check, anti-dupe races (needs `MARSSCAPE_TEST`; `--live` runs the part that doesn't).
- `test/stack.mjs`: inventory stacking: gathering, crafting from stacks (and a full pack), partial drops, eat / rub taking one, the locker, the shop, part-stack trades and the anti-dupe check, old saves merging on load (needs `MARSSCAPE_TEST`; `--live` runs the gathering and drop checks).
- `test/load.mjs`: load test. Autonomous clients over several rooms, reporting tick jitter, message rates, errors, CPU and SQLite rows written.

## Colonists

Each room keeps a few NPC colonists, so a site never feels empty:
- a **quartermaster** at the fabricator, who hands each new character a starter kit;
- a **guard**, who protects players from creatures that jump them;
- a **medic**, who heals anyone below half hp;
- a **miner** or **surveyor**, who works the nodes near spawn;
- a **trader** at her stall by the fabricator, who runs the shop. She is always there, on top of
  the crew below.

Population: there are about 4 with one human, one fewer per extra human, and never fewer than 1
(not counting the trader).

Chat commands:
- **`help`** (or `follow me`): a guard comes and escorts you for 2 minutes.
- **`stop`** (or `bye`): sends it back to its post.

Colonists use the same rules as players and are never saved. A room still stops ticking when its
last human leaves. The details are in [PROTOCOL.md](PROTOCOL.md#colonists-extra).

Colonists also hand out quests: `talk` to one. There are eight, ending in "Echoes Below", which
sends you down the pit into the lava tube for an ancient relic and unlocks relic armour.

## The cave, the bank, trading and Rift Surges

- **Lava tube:** every site has a pit near the landing site. It leads to `<site>~cave`, a second
  room with a server-generated tube (walls enforced), richer nodes and tougher creatures. Your
  character goes down and comes back up with you: the cave borrows it from the surface room over
  Durable Object RPC, so it is never in two places.
- **Colony locker:** a 100-slot bank next to the fabricator. Every change is saved at once, in the same
  step as your inventory.
- **Trading:** two-step accept/confirm, checked and swapped atomically, logged.
- **The trader's shop:** credits (from the starter kit, every creature kill and some quests) buy
  medkits, water, bricks, plates, a chitin helmet, blasters and Insight Lamps from a shelf the whole
  room shares. She buys almost anything for less than she sells it, and prices follow her stock,
  so there's no buy-and-sell-back profit. Your last 10 sales can be bought back at exactly what she
  paid, for 30 minutes. The Rift Surge trophies can't be sold.
- **Armour:** helmets, suits, a shield and a cape, crafted at the fabricator. The armour bonus raises
  your defence roll.
- **Rift Surge:** every 20–30 minutes while people play, a big rift opens near the landing site,
  waves pour out, then a Void Leviathan. Everyone who helps gets a loot roll. Surges only run while
  humans are present.

## Run locally

```sh
cd server
npm install
npx wrangler dev --port 8787 --var MARSSCAPE_TEST:1   # serves http://127.0.0.1:8787, ws://127.0.0.1:8787/room/<site>
node test/bots.mjs                  # in a second terminal; prints PASS/FAIL, takes about 1 minute
node test/community.mjs             # colonists; about 1-2 minutes; the medic check needs MARSSCAPE_TEST
node test/features.mjs              # cave, bank, trading, quests, armour, surge; 3-5 minutes; needs MARSSCAPE_TEST
node test/shop.mjs                  # the trader's shop; about 2 minutes; needs MARSSCAPE_TEST
node test/stack.mjs                 # inventory stacking; about 2 minutes; needs MARSSCAPE_TEST
node test/load.mjs ws://127.0.0.1:8787 30 3 300   # 30 clients over 3 rooms for 5 minutes
```

`MARSSCAPE_TEST=1` only enables a dev `test` message (set hp or skills, give items, force a Rift
Surge, sample hit rates), which the tests use. Never set it in `wrangler.toml`.

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
- Each site is its own world, so characters don't carry between sites (they do carry into that site's cave).
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
| SQLite writes | about 1 row per player save (every 10 ticks while a player changes) and 1 per room meta write (every 10 ticks); since 2026-10-31 much less, see below |

The 10 errors in the run were the load clients sending paths from a stale position ("Path steps must be adjacent tiles.").

**Free plan** (Durable Objects: 100k requests/day, 13,000 GB-s/day, 100k SQLite rows written/day; resets 00:00 UTC; over a limit, those operations fail until the reset). Worked example: **20 humans, 4 hours a day**.

| Limit | What uses it | 20 humans × 4 h/day |
|---|---|---|
| **Duration** (the binding limit) | A room is billed 128 MB for every second it ticks. 13,000 GB-s ≈ **29 room-hours a day**. | 1 room = 1,800 GB-s (14%); 5 rooms = 9,000 (69%); 20 solo rooms = 36,000 (**277%**, they stop after about 1.4 h) |
| **Rows written** | About 110/h per active player (was 600) plus almost nothing per room (was 600/h) | about 9k (9%), and still under 10% with 20 solo rooms |
| **Requests** | Each WebSocket connect = 1. Incoming messages are billed 20:1. | 20 × 14,400 s × 0.7 msg/s ÷ 20 ≈ 10k/day (10%) |
| Rows read, storage | Logins and gift checks only; tiny | — |

**Risks:**
- **Many solo rooms:** players spread over lots of sites are what blows the free plan, through duration first and then rows written.
- **Rows written, fixed on 2026-10-31:** players are now saved every 50 ticks while they change (and on leave, travel, bank and trade), and the room's tick and id counter are written only when the room stops.

**Write rate, measured on 2026-10-31.** Local `wrangler dev`, `test/load.mjs` with 15 clients over 3 rooms for 240 s. The old code (main `982a0fe` plus the same row counter) and the new code ran side by side. `/info` now reports `rows_written`.

| | rows written per room (5 players) | per player |
|---|---|---|
| Before | about 3,540 / hour | about 709 / hour |
| After | about 540 / hour | about 108 / hour |

That is about 6.5× fewer. "20 humans × 4 h/day" now costs about 8–9k rows a day (about 9% of the free 100k), down from about 50%.
In the same run, ticks were p50 606 ms and p99 621 ms in every room, with no disconnects. Both servers together used 7% of one core.
