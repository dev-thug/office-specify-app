import { describe, expect, it } from "vitest";

import {
  emergencyStopFormValid,
  channelToggleDisabled,
} from "@/features/operations/gate-rules";
import { jobPrimaryAction } from "@/features/operations/job-rules";

describe("jobPrimaryAction", () => {
  it("never offers retry for unknown — investigate only, even if server says retryable", () => {
    expect(jobPrimaryAction({ state: "unknown", retryable: true })).toBe("investigate");
    expect(jobPrimaryAction({ state: "unknown" })).toBe("investigate");
  });

  it("offers retry only for dead/canceled jobs the server marks retryable", () => {
    expect(jobPrimaryAction({ state: "dead", retryable: true })).toBe("retry");
    expect(jobPrimaryAction({ state: "canceled", retryable: true })).toBe("retry");
  });

  it("disables retry for dead jobs without the server retryable flag", () => {
    expect(jobPrimaryAction({ state: "dead", retryable: false })).toBe("none");
    expect(jobPrimaryAction({ state: "dead" })).toBe("none");
  });

  it("disables retry for in-flight and completed states", () => {
    for (const state of ["queued", "running", "retry_wait", "completed"]) {
      expect(jobPrimaryAction({ state, retryable: true })).toBe("none");
    }
  });
});

describe("channelToggleDisabled (provider/policy gate)", () => {
  it("blocks enabling a channel whose policy is not allowed", () => {
    for (const policy of ["unverified", "blocked"]) {
      expect(
        channelToggleDisabled({ policy_status: policy, enabled: false }, { isAdmin: true })
      ).toBe(true);
    }
  });

  it("lets admin enable an allowed channel and always lets them disable", () => {
    expect(
      channelToggleDisabled({ policy_status: "allowed", enabled: false }, { isAdmin: true })
    ).toBe(false);
    expect(
      channelToggleDisabled({ policy_status: "blocked", enabled: true }, { isAdmin: true })
    ).toBe(false);
  });

  it("blocks everything for non-admin or while a save is in flight", () => {
    expect(
      channelToggleDisabled({ policy_status: "allowed", enabled: false }, { isAdmin: false })
    ).toBe(true);
    expect(
      channelToggleDisabled(
        { policy_status: "allowed", enabled: false },
        { isAdmin: true, pending: true }
      )
    ).toBe(true);
  });
});

describe("emergencyStopFormValid", () => {
  it("requires a non-blank reason and the exact confirm word", () => {
    expect(emergencyStopFormValid("attack", "중지", "중지")).toBe(true);
    expect(emergencyStopFormValid("   ", "중지", "중지")).toBe(false);
    expect(emergencyStopFormValid("attack", "중지!", "중지")).toBe(false);
    expect(emergencyStopFormValid("attack", "", "중지")).toBe(false);
  });
});
