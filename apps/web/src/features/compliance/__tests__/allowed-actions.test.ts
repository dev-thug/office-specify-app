import { describe, expect, it } from "vitest";

import {
  canRequestRelease,
  canReviewBasis,
  canRevokeBasis,
  hasAction,
} from "@/features/compliance";
import type { CommunicationBasis, Suppression } from "@/lib/api/compliance";

function suppression(over: Partial<Suppression> = {}): Suppression {
  return {
    id: "s1",
    email_address_id: "ea1",
    email_address: "user@example.com",
    purpose_scope: "marketing",
    reason: "unsubscribe",
    source_outbound_id: null,
    created_by: null,
    released_at: null,
    released_by: null,
    release_evidence_ref: null,
    version: 1,
    created_at: "2026-10-01T00:00:00Z",
    ...over,
  };
}

function basis(over: Partial<CommunicationBasis> = {}): CommunicationBasis {
  return {
    id: "b1",
    email_address_id: "ea1",
    purpose: "marketing",
    jurisdiction: "KR",
    basis_type: "explicit_consent",
    status: "pending",
    evidence_ref: "ev-1",
    collected_at: "2026-10-01T00:00:00Z",
    reviewed_by: null,
    reviewed_at: null,
    valid_until: null,
    revoked_at: null,
    version: 1,
    created_at: "2026-10-01T00:00:00Z",
    ...over,
  };
}

describe("canRequestRelease", () => {
  it("allows active suppressions", () => {
    expect(canRequestRelease(suppression())).toBe(true);
  });

  it("never offers release on an already-released row", () => {
    expect(
      canRequestRelease(
        suppression({
          released_at: "2026-10-02T00:00:00Z",
          released_by: "p1",
          release_evidence_ref: "ev",
        })
      )
    ).toBe(false);
  });

  it("hides the action when allowed_actions omits it", () => {
    expect(canRequestRelease(suppression({ allowed_actions: [] }))).toBe(false);
    expect(
      canRequestRelease(suppression({ allowed_actions: ["release_request"] }))
    ).toBe(true);
  });
});

describe("basis review/revoke state logic", () => {
  it("pending basis can be reviewed, not revoked", () => {
    const b = basis();
    expect(canReviewBasis(b)).toBe(true);
    expect(canRevokeBasis(b)).toBe(false);
  });

  it("valid basis can be revoked, not re-reviewed", () => {
    const b = basis({
      status: "valid",
      reviewed_by: "p1",
      reviewed_at: "2026-10-02T00:00:00Z",
    });
    expect(canReviewBasis(b)).toBe(false);
    expect(canRevokeBasis(b)).toBe(true);
  });

  it.each(["rejected", "revoked", "expired"] as const)(
    "status %s offers neither action",
    (status) => {
      const b = basis({ status });
      expect(canReviewBasis(b)).toBe(false);
      expect(canRevokeBasis(b)).toBe(false);
    }
  );

  it("honors allowed_actions as a hint when present", () => {
    expect(canReviewBasis(basis({ allowed_actions: [] }))).toBe(false);
    expect(canReviewBasis(basis({ allowed_actions: ["review"] }))).toBe(true);
    expect(canRevokeBasis(basis({ status: "valid", allowed_actions: [] }))).toBe(
      false
    );
  });
});

describe("hasAction", () => {
  it("treats missing allowed_actions as permitted (server re-checks)", () => {
    expect(hasAction(suppression(), "release_request")).toBe(true);
  });
});
