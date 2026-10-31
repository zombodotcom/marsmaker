// The lava tube under each site: a separate room ("<site>~cave") with a deterministic, twisty tube
// layout. The server owns the walls (the surface has none), so cave movement is checked against
// them. Layout, nodes and creatures depend only on the surface site id.

import { WorldNode, WorldNpc, NpcKind, fnv1a, mulberry32 } from "./game";

export const CAVE_W = 120;
export const CAVE_H = 120;
export const CAVE_SUFFIX = "~cave";

export interface CaveMap {
  w: number;
  h: number;
  grid: Uint8Array;          // 1 = floor, 0 = wall; index = tz * w + tx
  entrance: [number, number]; // the ladder tile
  spawn: [number, number];    // where you arrive, next to the ladder
  nodes: WorldNode[];         // id 0 = ladder
  npcs: WorldNpc[];
  rle: string;
}

export const isCaveSite = (site: string) => site.endsWith(CAVE_SUFFIX);
export const surfaceOf = (site: string) => (isCaveSite(site) ? site.slice(0, -CAVE_SUFFIX.length) : site);

export function floorAt(c: CaveMap, tx: number, tz: number): boolean {
  return tx >= 0 && tz >= 0 && tx < c.w && tz < c.h && c.grid[tz * c.w + tx] === 1;
}

/** A step between 8-neighbours is open if the target is floor and, for a diagonal, both corners are. */
export function stepOpen(c: CaveMap, x: number, z: number, nx: number, nz: number): boolean {
  if (!floorAt(c, nx, nz)) return false;
  if (nx !== x && nz !== z) return floorAt(c, nx, z) && floorAt(c, x, nz);
  return true;
}

/** Run lengths over the grid in row-major order, alternating wall/floor and starting with wall. */
export function encodeRle(grid: Uint8Array): string {
  const runs: number[] = [];
  let cur = 0, n = 0;
  for (const v of grid) {
    if (v === cur) n++;
    else { runs.push(n); cur = v; n = 1; }
  }
  runs.push(n);
  return runs.join(",");
}

export function generateCave(site: string): CaveMap {
  const rand = mulberry32(fnv1a("marsscape-cave:" + site));
  const w = CAVE_W, h = CAVE_H;
  const g = new Uint8Array(w * h);
  const carve = (cx: number, cz: number, r: number) => {
    const rr = r * r + 0.5;
    for (let dz = -r; dz <= r; dz++) for (let dx = -r; dx <= r; dx++) {
      if (dx * dx + dz * dz > rr) continue;
      const x = cx + dx, z = cz + dz;
      if (x >= 1 && z >= 1 && x < w - 1 && z < h - 1) g[z * w + x] = 1;
    }
  };
  const tube: [number, number][] = [];
  const walk = (x: number, z: number, ang: number, steps: number) => {
    for (let i = 0; i < steps; i++) {
      ang += (rand() - 0.5) * 0.7;
      let nx = x + Math.cos(ang), nz = z + Math.sin(ang);
      if (nx < 5 || nx > w - 6 || nz < 5 || nz > h - 6) {
        ang += Math.PI * (0.5 + rand() * 0.5); // bounce off the edge
        nx = Math.min(w - 6, Math.max(5, nx));
        nz = Math.min(h - 6, Math.max(5, nz));
      }
      x = nx; z = nz;
      const tx = Math.round(x), tz = Math.round(z);
      carve(tx, tz, rand() < 0.3 ? 2 : 1);
      tube.push([tx, tz]);
    }
  };
  const entrance: [number, number] = [6, 50 + Math.floor(rand() * 20)];
  carve(entrance[0], entrance[1], 3);
  walk(entrance[0], entrance[1], 0, 300);
  const branches = 3 + Math.floor(rand() * 2);
  for (let b = 0; b < branches; b++) {
    const [bx, bz] = tube[Math.floor(rand() * tube.length)];
    walk(bx, bz, rand() * Math.PI * 2, 60 + Math.floor(rand() * 70));
  }
  // chambers along the tubes, spread out
  const chambers: [number, number, number][] = [];
  for (let tries = 0; chambers.length < 8 && tries < 200; tries++) {
    const [cx, cz] = tube[Math.floor(rand() * tube.length)];
    if (Math.hypot(cx - entrance[0], cz - entrance[1]) < 15) continue;
    if (chambers.some(([x, z]) => Math.hypot(cx - x, cz - z) < 16)) continue;
    const r = 4 + Math.floor(rand() * 3);
    carve(cx, cz, r);
    chambers.push([cx, cz, r]);
  }
  // one smoothing pass: fill pockets in the walls, knock out lone pillars
  const s = g.slice();
  for (let z = 1; z < h - 1; z++) for (let x = 1; x < w - 1; x++) {
    let nb = 0;
    for (let dz = -1; dz <= 1; dz++) for (let dx = -1; dx <= 1; dx++) if ((dx || dz) && g[(z + dz) * w + x + dx]) nb++;
    const i = z * w + x;
    if (!g[i] && nb >= 6) s[i] = 1;
    else if (g[i] && nb <= 1) s[i] = 0;
  }
  // keep only what is reachable from the entrance (with the diagonal-corner rule)
  const grid = new Uint8Array(w * h);
  const q: number[] = [entrance[1] * w + entrance[0]];
  grid[q[0]] = 1;
  const tmp: CaveMap = { w, h, grid: s, entrance, spawn: entrance, nodes: [], npcs: [], rle: "" };
  while (q.length) {
    const i = q.pop()!, x = i % w, z = (i - x) / w;
    for (let dz = -1; dz <= 1; dz++) for (let dx = -1; dx <= 1; dx++) {
      if (!dx && !dz) continue;
      const j = (z + dz) * w + x + dx;
      if (grid[j] || !stepOpen(tmp, x, z, x + dx, z + dz)) continue;
      grid[j] = 1;
      q.push(j);
    }
  }
  const cave: CaveMap = { w, h, grid, entrance, spawn: [entrance[0] + 1, entrance[1]], nodes: [], npcs: [], rle: encodeRle(grid) };

  // nodes: ladder (id 0), void veins and fossils in the chambers away from the entrance
  const used = new Set<string>([`${entrance[0]},${entrance[1]}`, `${cave.spawn[0]},${cave.spawn[1]}`]);
  cave.nodes.push({ id: 0, kind: "ladder", tx: entrance[0], tz: entrance[1] });
  const spot = (cx: number, cz: number, r: number): [number, number] | null => {
    for (let t = 0; t < 40; t++) {
      const x = cx + Math.round((rand() * 2 - 1) * r), z = cz + Math.round((rand() * 2 - 1) * r);
      if (floorAt(cave, x, z) && !used.has(`${x},${z}`)) { used.add(`${x},${z}`); return [x, z]; }
    }
    return null;
  };
  const far = chambers.filter(([x, z]) => floorAt(cave, x, z))
    .sort((a, b) => Math.hypot(a[0] - entrance[0], a[1] - entrance[1]) - Math.hypot(b[0] - entrance[0], b[1] - entrance[1]));
  let veins = 0, fossils = 0;
  far.forEach(([cx, cz, r], k) => {
    for (let i = 0; i < 2 && veins < 12; i++) {
      const p = spot(cx, cz, r - 1);
      if (p) { cave.nodes.push({ id: cave.nodes.length, kind: "void_vein", tx: p[0], tz: p[1] }); veins++; }
    }
    for (let i = 0; i < (k % 2 ? 2 : 1) && fossils < 8; i++) {
      const p = spot(cx, cz, r - 1);
      if (p) { cave.nodes.push({ id: cave.nodes.length, kind: "fossil", tx: p[0], tz: p[1] }); fossils++; }
    }
  });

  // creatures: glowworms along the tubes, crystal horrors in the far chambers
  const addNpc = (kind: NpcKind, x: number, z: number) => cave.npcs.push({ id: cave.npcs.length + 1, kind, home: [x, z] });
  const tubeFloor = tube.filter(([x, z]) => floorAt(cave, x, z) && Math.hypot(x - entrance[0], z - entrance[1]) > 15);
  for (let i = 0; i < 14 && tubeFloor.length; i++) {
    const [x, z] = tubeFloor[Math.floor(rand() * tubeFloor.length)];
    addNpc("glowworm", x, z);
  }
  far.slice(Math.floor(far.length / 2)).forEach(([cx, cz, r]) => {
    const p = spot(cx, cz, r - 1);
    if (p) addNpc("crystal_horror", p[0], p[1]);
  });
  return cave;
}
