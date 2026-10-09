import { describe, expect, it } from "vitest";

import { DEFAULT_AFTER_LOGIN, safeNextPath } from "@/lib/safe-next";

describe("safeNextPath", () => {
  it("accepts same-origin /app paths", () => {
    expect(safeNextPath("/app/dashboard")).toBe("/app/dashboard");
    expect(safeNextPath("/app/inbox/thread-1?tab=notes")).toBe(
      "/app/inbox/thread-1?tab=notes"
    );
  });

  it("rejects external and protocol-relative URLs", () => {
    expect(safeNextPath("//evil.com/app/dashboard")).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("https://evil.com/app")).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("javascript:alert(1)")).toBe(DEFAULT_AFTER_LOGIN);
  });

  it("rejects non-/app paths", () => {
    expect(safeNextPath("/login")).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("/public/x")).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("/")).toBe(DEFAULT_AFTER_LOGIN);
    // "/app" 접두 우회 — /app 바로 뒤에 / 없이 오는 경로는 거부
    expect(safeNextPath("/application")).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("/appx/../login")).toBe(DEFAULT_AFTER_LOGIN);
    // 정규화 시 /app 탈출
    expect(safeNextPath("/app/../admin")).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("/app/%2e%2e/login")).toBe(DEFAULT_AFTER_LOGIN);
  });

  it("rejects empty/missing values", () => {
    expect(safeNextPath(null)).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath(undefined)).toBe(DEFAULT_AFTER_LOGIN);
    expect(safeNextPath("")).toBe(DEFAULT_AFTER_LOGIN);
  });
});
