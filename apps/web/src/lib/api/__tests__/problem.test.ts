import { describe, expect, it } from "vitest";

import { isProblemContentType, parseProblem, ApiError } from "@/lib/api/problem";

describe("parseProblem", () => {
  it("parses a full problem+json body", () => {
    const p = parseProblem(409, {
      type: "urn:outreach:problem:version-conflict",
      title: "Version conflict",
      status: 409,
      code: "version_conflict",
      detail: "row changed",
      request_id: "req-1",
      retryable: false,
      violations: [{ field: "name", code: "changed", detail: "name differs" }],
    });
    expect(p.status).toBe(409);
    expect(p.code).toBe("version_conflict");
    expect(p.requestId).toBe("req-1");
    expect(p.retryable).toBe(false);
    expect(p.violations).toEqual([
      { field: "name", code: "changed", detail: "name differs" },
    ]);
  });

  it("falls back on non-object bodies and keeps status", () => {
    const p = parseProblem(503, null, { requestId: "req-2" });
    expect(p.status).toBe(503);
    expect(p.code).toBe("INTERNAL");
    expect(p.requestId).toBe("req-2");
  });

  it("maps 429 to RATE_LIMITED by default", () => {
    expect(parseProblem(429, null).code).toBe("RATE_LIMITED");
  });

  it("prefers header request id when body omits it", () => {
    const p = parseProblem(400, { title: "Bad" }, { requestId: "req-hdr" });
    expect(p.requestId).toBe("req-hdr");
    expect(p.title).toBe("Bad");
    expect(p.code).toBe("UNKNOWN");
  });
});

describe("isProblemContentType", () => {
  it("matches problem+json with parameters", () => {
    expect(isProblemContentType('application/problem+json; charset=utf-8')).toBe(true);
    expect(isProblemContentType("application/json")).toBe(false);
    expect(isProblemContentType(null)).toBe(false);
  });
});

describe("ApiError", () => {
  it("exposes status/code/requestId getters", () => {
    const err = new ApiError(
      parseProblem(412, { code: "version_conflict", detail: "stale", request_id: "r" })
    );
    expect(err.status).toBe(412);
    expect(err.code).toBe("version_conflict");
    expect(err.requestId).toBe("r");
    expect(err.message).toBe("stale");
  });
});
