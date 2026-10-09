import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { NAMESPACES } from "@/i18n/namespaces";
import { routing } from "@/i18n/routing";

// vitest 는 apps/web 에서 실행된다.
const MESSAGES_DIR = join(process.cwd(), "messages");

function readNamespace(locale: string, ns: string): unknown {
  return JSON.parse(
    readFileSync(`${MESSAGES_DIR}/${locale}/${ns}.json`, "utf-8")
  ) as unknown;
}

/** 객체를 펼쳐 "a.b.c" 경로 목록으로 만든다 — 비객체/빈 객체는 말단으로 친다. */
function flattenKeys(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return [prefix];
  }
  const keys = Object.keys(value as Record<string, unknown>);
  if (keys.length === 0) return [prefix];
  return keys.flatMap((key) =>
    flattenKeys(
      (value as Record<string, unknown>)[key],
      prefix ? `${prefix}.${key}` : key
    )
  );
}

describe("i18n 네임스페이스 파일", () => {
  it("각 로케일 디렉터리가 NAMESPACES 의 <ns>.json 을 정확히 갖는다", () => {
    const expected = [...NAMESPACES].map((ns) => `${ns}.json`).sort();
    for (const locale of routing.locales) {
      const files = readdirSync(`${MESSAGES_DIR}/${locale}`)
        .filter((f) => f.endsWith(".json"))
        .sort();
      expect(files).toEqual(expected);
    }
  });

  it.each(NAMESPACES)("ko/en 키 패리티 — %s.json", (ns) => {
    const ko = new Set(flattenKeys(readNamespace("ko", ns)));
    const en = new Set(flattenKeys(readNamespace("en", ns)));
    const onlyKo = [...ko].filter((k) => !en.has(k));
    const onlyEn = [...en].filter((k) => !ko.has(k));
    expect({ onlyKo, onlyEn }).toEqual({ onlyKo: [], onlyEn: [] });
  });

  it("company lifecycle stages are directly translatable in both locales", () => {
    const stages = ["prospect", "engaged", "qualified", "customer", "inactive"];
    for (const locale of routing.locales) {
      const companies = readNamespace(locale, "companies") as {
        stages?: Record<string, unknown>;
      };
      for (const stage of stages) {
        expect(companies.stages?.[stage], `${locale}: companies.stages.${stage}`).toEqual(expect.any(String));
        expect(companies.stages?.[stage]).not.toBe("");
      }
    }
  });
});
