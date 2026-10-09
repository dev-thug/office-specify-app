import { afterEach, describe, expect, it, vi } from "vitest";

import {
  decideLeadProposal,
  leadProposalDecisionsPath,
  researchEvidenceReviewsPath,
  reviewResearchEvidence,
} from "@/lib/api/research";

function mockFetch(status = 200, body: unknown = { data: {} }) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("research api", () => {
  it("decides a proposal with If-Match from version and an idempotency key", async () => {
    const calls = mockFetch(200, { data: { id: "p1" } });
    await decideLeadProposal(
      "p1",
      { decision: "accept", merge_into_company_id: "c9", reason: "ok" },
      { etag: 5, idempotencyKey: "dec-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/lead-proposals/p1/decisions");
    expect(h.get("If-Match")).toBe('"5"');
    expect(h.get("Idempotency-Key")).toBe("dec-key");
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      decision: "accept",
      merge_into_company_id: "c9",
      reason: "ok",
    });
  });

  it("reviews evidence without If-Match (research_evidence has no version)", async () => {
    const calls = mockFetch(200, { data: { id: "e1" } });
    await reviewResearchEvidence(
      "e1",
      { review_status: "verified" },
      { idempotencyKey: "rev-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/research-evidence/e1/reviews");
    expect(h.get("If-Match")).toBeNull();
    expect(h.get("Idempotency-Key")).toBe("rev-key");
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      review_status: "verified",
    });
  });

  it("builds paths", () => {
    expect(leadProposalDecisionsPath("p7")).toBe(
      "/api/v1/lead-proposals/p7/decisions"
    );
    expect(researchEvidenceReviewsPath("e7")).toBe(
      "/api/v1/research-evidence/e7/reviews"
    );
  });
});
