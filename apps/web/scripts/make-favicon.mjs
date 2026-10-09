#!/usr/bin/env node
/**
 * src/app/favicon.ico 생성 — logo.png를 sips로 16/32 PNG로 줄인 뒤
 * ICO 컨테이너(PNG payload, Vista+ 방식)로 포장한다.
 * 사용: node scripts/make-favicon.mjs
 */
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const src = join(root, "public/logo.png");
const out = join(root, "src/app/favicon.ico");

const tmp = mkdtempSync(join(tmpdir(), "favicon-"));
const sizes = [16, 32];
const pngs = sizes.map((size) => {
  const p = join(tmp, `icon-${size}.png`);
  execFileSync("sips", ["-z", String(size), String(size), src, "--out", p], {
    stdio: "ignore",
  });
  return { size, data: readFileSync(p) };
});

// ICONDIR(6B) + ICONDIRENTRY(16B × N) + PNG payloads
const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0); // reserved
header.writeUInt16LE(1, 2); // type: icon
header.writeUInt16LE(pngs.length, 4);

let offset = 6 + 16 * pngs.length;
const entries = pngs.map(({ size, data }) => {
  const e = Buffer.alloc(16);
  e.writeUInt8(size === 256 ? 0 : size, 0); // width (0=256)
  e.writeUInt8(size === 256 ? 0 : size, 1); // height
  e.writeUInt8(0, 2); // palette
  e.writeUInt8(0, 3); // reserved
  e.writeUInt16LE(1, 4); // planes
  e.writeUInt16LE(32, 6); // bpp
  e.writeUInt32LE(data.length, 8);
  e.writeUInt32LE(offset, 12);
  offset += data.length;
  return e;
});

writeFileSync(out, Buffer.concat([header, ...entries, ...pngs.map((p) => p.data)]));
console.log(`wrote ${out} (${pngs.map((p) => p.size).join("+")}px)`);
