import { describe, expect, it } from "vitest";

import {
  canCancelRun,
  isActiveRunStatus,
  isFinalRunStatus,
} from "@/features/agents";

describe("run-state", () => {
  it("treats queued/running/awaiting_approval as active", () => {
    for (const s of ["queued", "running", "awaiting_approval"]) {
      expect(isActiveRunStatus(s)).toBe(true);
      expect(isFinalRunStatus(s)).toBe(false);
    }
  });

  it("treats completed/failed/canceled as final and inactive", () => {
    for (const s of ["completed", "failed", "canceled"]) {
      expect(isFinalRunStatus(s)).toBe(true);
      expect(isActiveRunStatus(s)).toBe(false);
    }
  });

  it("allows cancel only while active and not already requested", () => {
    expect(canCancelRun({ status: "running" })).toBe(true);
    expect(
      canCancelRun({ status: "running", cancel_requested_at: "2026-10-09T00:00:00Z" })
    ).toBe(false);
    expect(canCancelRun({ status: "completed" })).toBe(false);
    expect(canCancelRun({ status: "failed" })).toBe(false);
  });

  it("honours allowed_actions when the server sends them", () => {
    expect(
      canCancelRun({ status: "queued", allowed_actions: ["cancel"] })
    ).toBe(true);
    expect(canCancelRun({ status: "queued", allowed_actions: [] })).toBe(false);
  });
});
