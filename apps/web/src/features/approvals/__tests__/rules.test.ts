import { describe, expect, it } from "vitest";

import {
  approvalActions,
  approvalExpired,
  approvalStateKey,
  isSelfRequest,
} from "@/features/approvals/rules";
import type { ApprovalListItem } from "@/lib/api/approvals";

const base: ApprovalListItem = {
  id: "r1",
  action_type: "campaign_launch",
  status: "pending",
  requested_by: "user-a",
  version: 1,
};

describe("approvalExpired / isSelfRequest", () => {
  it("만료 시각이 지났거나 status=expired면 만료", () => {
    const now = new Date("2026-10-10T00:00:00Z");
    expect(approvalExpired(null, now)).toBe(false);
    expect(approvalExpired({ status: "expired" }, now)).toBe(true);
    expect(
      approvalExpired(
        { status: "pending", expires_at: "2026-10-09T00:00:00Z" },
        now
      )
    ).toBe(true);
    expect(
      approvalExpired(
        { status: "pending", expires_at: "2026-10-11T00:00:00Z" },
        now
      )
    ).toBe(false);
  });

  it("본인 요청 판별 — self_request 힌트 우선, 없으면 requested_by 비교", () => {
    expect(isSelfRequest(base, "user-a")).toBe(true);
    expect(isSelfRequest(base, "user-b")).toBe(false);
    // 힌트가 false면 requested_by가 같아도 본인 아님으로 간주(서버가 진실)
    expect(isSelfRequest({ ...base, self_request: false }, "user-a")).toBe(
      false
    );
    expect(isSelfRequest({ ...base, self_request: true }, "user-b")).toBe(true);
    expect(isSelfRequest(base, undefined)).toBe(false);
  });
});

describe("approvalActions", () => {
  it("pending + 남의 요청 → 승인·거절 버튼 표시", () => {
    const a = approvalActions(base, { mePrincipalId: "user-b" });
    expect(a.approve.visible).toBe(true);
    expect(a.reject.visible).toBe(true);
    expect(a.revoke.visible).toBe(false);
  });

  it("본인 요청에는 승인·거절 버튼을 렌더하지 않는다", () => {
    const a = approvalActions(base, { mePrincipalId: "user-a" });
    expect(a.approve.visible).toBe(false);
    expect(a.reject.visible).toBe(false);
  });

  it("만료된 pending은 결정 불가", () => {
    const a = approvalActions(
      { ...base, expires_at: "2000-01-01T00:00:00Z" },
      { mePrincipalId: "user-b", now: new Date("2026-10-10T00:00:00Z") }
    );
    expect(a.approve.visible).toBe(false);
    expect(a.reject.visible).toBe(false);
  });

  it("rejected/expired/revoked는 결정·철회 모두 불가", () => {
    for (const s of ["rejected", "expired", "revoked"]) {
      const a = approvalActions(
        { ...base, status: s },
        { mePrincipalId: "user-b" }
      );
      expect(a.approve.visible).toBe(false);
      expect(a.reject.visible).toBe(false);
      expect(a.revoke.visible).toBe(false);
    }
  });

  it("approved는 철회만 가능", () => {
    const a = approvalActions(
      { ...base, status: "approved" },
      { mePrincipalId: "user-b" }
    );
    expect(a.approve.visible).toBe(false);
    expect(a.revoke.visible).toBe(true);
  });

  it("can_decide=false 힌트는 결정 버튼을 숨긴다", () => {
    const a = approvalActions(
      { ...base, can_decide: false },
      { mePrincipalId: "user-b" }
    );
    expect(a.approve.visible).toBe(false);
  });

  it("allowed_actions에 decide가 없으면 숨긴다", () => {
    const a = approvalActions(
      { ...base, allowed_actions: ["revoke"] },
      { mePrincipalId: "user-b" }
    );
    expect(a.approve.visible).toBe(false);
    expect(a.reject.visible).toBe(false);
  });
});

describe("approvalStateKey", () => {
  it("상태 → 배너 메시지 키", () => {
    expect(approvalStateKey("approved")).toBe("stateApproved");
    expect(approvalStateKey("rejected")).toBe("stateRejected");
    expect(approvalStateKey("expired")).toBe("stateExpired");
    expect(approvalStateKey("revoked")).toBe("stateRevoked");
    expect(approvalStateKey("pending")).toBe("statePending");
    expect(approvalStateKey("weird")).toBeNull();
  });
});
