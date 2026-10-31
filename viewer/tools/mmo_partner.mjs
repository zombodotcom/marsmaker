// MarsScape test partner: a scripted second player for testing trades against a real server.
//   node tools/mmo_partner.mjs wss://marsscape.tommysciano.workers.dev SITE_ID [name=Partner] [seconds=120]
// Joins the room, waits for the quartermaster's starter kit, then answers any trade request:
// offers a brick (or nothing), accepts both screens, and prints how the trade ended.
// Needs Node 22+ (global WebSocket).

const [BASE, SITE, NAME = "Partner", SECS = "120"] = process.argv.slice(2);
if (!BASE || !SITE) {
  console.error("usage: node tools/mmo_partner.mjs WS_URL SITE_ID [name] [seconds]");
  process.exit(2);
}
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);
const ws = new WebSocket(`${BASE.replace(/\/$/, "")}/room/${SITE}`);
let inv = [], offered = false, tradeTimer = null;
const send = (m) => ws.send(JSON.stringify(m));
const later = (ms, f) => setTimeout(f, ms);

ws.onopen = () => send({ t: "hello", ver: 1, name: NAME, token: "", bounds: [-4096, -4096, 4096, 4096], spawn: [0, 0] });
ws.onclose = (e) => { log("closed", e.code, e.reason); process.exit(0); };
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  switch (m.t) {
    case "welcome":
      inv = m.you.inv;
      log(`welcome: id ${m.id} at ${m.you.tx},${m.you.tz}; ${m.players.length} others here`);
      break;
    case "inv":
      inv = m.inv;
      break;
    case "trade_request":
      log(`trade request from ${m.name} (${m.from}): accepting`);
      later(800, () => send({ t: "trade_request", id: m.from }));
      break;
    case "trade": {
      log(`trade stage ${m.stage}: mine ${JSON.stringify(m.mine)} theirs ${JSON.stringify(m.theirs)} accepted ${JSON.stringify(m.accepted)}`);
      if (m.stage === 1 && !offered) {
        offered = true;
        const slot = inv.findIndex((s) => s && s.item === "brick");
        if (slot >= 0) later(1000, () => send({ t: "trade_offer", slot, qty: 1 }));
      }
      // accept a little after every change (each change resets both accepts)
      if (!m.accepted.me) {
        clearTimeout(tradeTimer);
        tradeTimer = later(m.stage === 1 ? 4000 : 2500, () => send({ t: "trade_accept" }));
      }
      break;
    }
    case "trade_end":
      log(`trade ended: ${m.result} ${m.reason ?? ""}`);
      break;
    case "msg":
    case "err":
      log(`${m.t}: ${m.text}`);
      break;
  }
};
later(Number(SECS) * 1000, () => { log("time up"); ws.close(); });
