import { describe, expect, it } from "vitest";

import {
  applyGrantChange,
  grantBlockReason,
  type GrantKey,
} from "@/features/settings/grant-rules";
import type { GrantBooleans } from "@/lib/api/settings";

const OFF: GrantBooleans = {
  can_read: false,
  can_compose: false,
  can_send: false,
  can_approve: false,
  can_assign: false,
};

const ALL_ON: GrantBooleans = {
  can_read: true,
  can_compose: true,
  can_send: true,
  can_approve: true,
  can_assign: true,
};

describe("applyGrantChange — 의존 규칙", () => {
  it("turning on a child grants all ancestors (approve → send → compose → read; assign stays off)", () => {
    const next = applyGrantChange(OFF, "can_approve", true);
    // can_assign 은 approve 의 조상이 아니다 — DDL CHECK (NOT can_approve OR can_send).
    expect(next).toEqual({ ...ALL_ON, can_assign: false });
  });

  it("turning on compose also turns on read", () => {
    const next = applyGrantChange(OFF, "can_compose", true);
    expect(next.can_read).toBe(true);
    expect(next.can_compose).toBe(true);
    expect(next.can_send).toBe(false);
  });

  it("assign requires read", () => {
    const next = applyGrantChange(OFF, "can_assign", true);
    expect(next.can_read).toBe(true);
  });

  it("turning off a parent revokes every descendant", () => {
    const next = applyGrantChange(ALL_ON, "can_read", false);
    expect(next).toEqual(OFF);
  });

  it("turning off compose keeps read+assign but drops send+approve", () => {
    const next = applyGrantChange(ALL_ON, "can_compose", false);
    expect(next).toEqual({
      can_read: true,
      can_compose: false,
      can_send: false,
      can_approve: false,
      can_assign: true,
    });
  });
});

describe("grantBlockReason — 부여 불가 정책", () => {
  const agent = { kind: "agent", role: "agent" };
  const member = { kind: "human", role: "member" };
  const manager = { kind: "human", role: "manager" };
  const admin = { kind: "human", role: "admin" };

  it("agents can never hold send or approve", () => {
    const blocked: GrantKey[] = ["can_send", "can_approve"];
    for (const k of blocked) {
      expect(grantBlockReason(agent, k)).toBe("agent");
    }
    expect(grantBlockReason(agent, "can_read")).toBeNull();
    expect(grantBlockReason(agent, "can_compose")).toBeNull();
    expect(grantBlockReason(agent, "can_assign")).toBeNull();
  });

  it("approve is limited to admin/manager roles", () => {
    expect(grantBlockReason(member, "can_approve")).toBe("role");
    expect(grantBlockReason(manager, "can_approve")).toBeNull();
    expect(grantBlockReason(admin, "can_approve")).toBeNull();
  });

  it("other grants are unrestricted for human principals", () => {
    for (const k of ["can_read", "can_compose", "can_send", "can_assign"] as const) {
      expect(grantBlockReason(member, k)).toBeNull();
    }
  });
});
