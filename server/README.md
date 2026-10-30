# MarsScape server

MarsScape is the old-school-MMO mode of MarsMaker. It has:
- 600 ms ticks and tile movement;
- six skills: mining, survey, fabrication, ranged, defence and hitpoints;
- a 28-slot inventory, public chat, and demons to shoot;
- real HiRISE terrain, with one shared world per Mars site.

This folder is the server: a Cloudflare Worker with one Durable Object ("room") per site. Player
progress is stored in the room's SQLite storage. Everything runs on the Cloudflare free plan.
The wire protocol is in [PROTOCOL.md](PROTOCOL.md).

- `src/index.ts`: Worker routing plus the `Room` Durable Object (tick loop, validation, persistence).
- `src/game.ts`: game rules (XP curve, nodes, recipes, weapons, demons, world generation).
- `test/bots.mjs`: bot test. Bots chat, walk, mine, craft, drop, fight an imp, and reconnect.

## Run locally

```sh
cd server
npm install
npx wrangler dev --port 8787        # serves http://127.0.0.1:8787, ws://127.0.0.1:8787/room/<site>
node test/bots.mjs                  # in a second terminal; prints PASS/FAIL, takes about 1 minute
```

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
  connected, then hibernates.
