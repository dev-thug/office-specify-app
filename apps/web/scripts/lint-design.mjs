#!/usr/bin/env node
/**
 * 디자인 토큰 린트 — src/**의 ts/tsx에서 하드코딩된 색을 막는다 (spec §6.1).
 * globals.css(토큰 정의처)와 생성 스키마는 스캔 대상이 아니다.
 */
import { readdir, readFile } from "node:fs/promises";
import { join, relative } from "node:path";

const SRC = new URL("../src", import.meta.url).pathname;
const EXTENSIONS = new Set([".ts", ".tsx"]);

const RULES = [
  {
    name: "hex color",
    re: /#[0-9a-fA-F]{3,8}\b/g,
    hint: "하드코딩된 hex 대신 시맨틱 토큰(bg-primary 등)을 사용하세요.",
  },
  {
    name: "rgb()/hsl() color",
    re: /\b(?:rgb|rgba|hsl|hsla|oklch|oklab)\s*\(/g,
    hint: "색 함수 대신 시맨틱 토큰을 사용하세요.",
  },
  {
    name: "Tailwind default palette",
    // bg-red-500, text-gray-400, dark:ring-blue-200 같은 기본 팔레트 유틸.
    re: /(?:^|[\s"'`:=])(?:[a-zA-Z-]+:)*\s*(?:bg|text|border|ring|fill|stroke|from|via|to|divide|outline|decoration|accent|caret|placeholder|shadow|selection)-(?:slate|gray|zinc|neutral|stone|red|orange|yellow|lime|green|emerald|cyan|sky|blue|indigo|violet|purple|fuchsia|pink)-(?:5|50|100|200|300|400|500|600|700|800|900|950)\b/g,
    hint: "Tailwind 기본 팔레트 대신 시맨틱 토큰을 사용하세요 (teal/rose/amber 토큰은 suffix 없이 허용).",
  },
  {
    name: "white/black palette",
    re: /(?:^|[\s"'`:=])(?:[a-zA-Z-]+:)*\s*(?:bg|text|border|ring|fill|stroke|divide|from|via|to|outline|decoration)-(?:white|black)\b/g,
    hint: "white/black 대신 background/foreground 토큰을 사용하세요.",
  },
];

async function* walk(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) yield* walk(path);
    else if (EXTENSIONS.has(path.slice(path.lastIndexOf(".")))) yield path;
  }
}

let failures = 0;
for await (const file of walk(SRC)) {
  const content = await readFile(file, "utf8");
  const lines = content.split("\n");
  for (const rule of RULES) {
    rule.re.lastIndex = 0;
    lines.forEach((line, i) => {
      rule.re.lastIndex = 0;
      let m;
      while ((m = rule.re.exec(line)) !== null) {
        failures += 1;
        const rel = relative(new URL("..", import.meta.url).pathname, file);
        console.error(
          `${rel}:${i + 1}:${m.index + 1}  ${rule.name}  "${m[0].trim()}"\n    ${rule.hint}`
        );
      }
    });
  }
}

if (failures > 0) {
  console.error(`\nlint:design — ${failures}개 위반`);
  process.exit(1);
} else {
  console.log("lint:design — 위반 없음");
}
