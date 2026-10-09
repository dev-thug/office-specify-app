import { describe, expect, it } from "vitest";

import {
  canDecideProposal,
  displayValue,
  findUnverifiedEmails,
} from "@/features/research";

describe("findUnverifiedEmails", () => {
  it("finds email-shaped strings at the top level", () => {
    const found = findUnverifiedEmails({ contact_email: "a@b.co" });
    expect(found).toEqual([
      { path: "contact_email", value: "a@b.co" },
    ]);
  });

  it("finds emails nested in objects and arrays", () => {
    const found = findUnverifiedEmails({
      contacts: [
        { name: "김", email: "kim@x.io" },
        { email: "lee@x.io" },
      ],
      meta: { source: { mail: "deep@x.io" } },
    });
    const paths = found.map((f) => f.path);
    expect(paths).toContain("contacts[0].email");
    expect(paths).toContain("contacts[1].email");
    expect(paths).toContain("meta.source.mail");
  });

  it("flags values under an email-like key even if loosely formatted", () => {
    // 엄격한 이메일 형식은 아니지만 키가 mail 계열이고 @ 가 있으면 플래그한다.
    const found = findUnverifiedEmails({ primary_mail: "user@x" });
    expect(found).toHaveLength(1);
  });

  it("ignores non-email strings and numbers", () => {
    expect(
      findUnverifiedEmails({ name: "회사", size: 42, note: "no at sign" })
    ).toHaveLength(0);
  });

  it("stops recursing past the depth limit", () => {
    const deep: Record<string, unknown> = { v: "root@x.io" };
    let cur = deep;
    for (let i = 0; i < 10; i++) {
      cur.next = { v: `deep${i}@x.io` };
      cur = cur.next as Record<string, unknown>;
    }
    const found = findUnverifiedEmails(deep);
    // depth 제한(6) — 루트 포함 7단계까지의 이메일만 수집한다.
    expect(found.length).toBeLessThanOrEqual(7);
    expect(found.some((f) => f.value === "deep9@x.io")).toBe(false);
  });
});

describe("canDecideProposal", () => {
  it("allows decisions only on pending/needs_review", () => {
    expect(canDecideProposal("pending")).toBe(true);
    expect(canDecideProposal("needs_review")).toBe(true);
    expect(canDecideProposal("accepted")).toBe(false);
    expect(canDecideProposal("rejected")).toBe(false);
  });
});

describe("displayValue", () => {
  it("renders scalars and serializes objects", () => {
    expect(displayValue("x")).toBe("x");
    expect(displayValue(3)).toBe("3");
    expect(displayValue(null)).toBe("—");
    expect(displayValue({ a: 1 })).toBe('{"a":1}');
  });
});
