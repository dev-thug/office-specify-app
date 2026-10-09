import { describe, expect, it } from "vitest";

import {
  campaignActions,
  channelBlocked,
  channelGateReasons,
  latestRevision,
  launchActions,
  passedRecipientIds,
  summarizePreflight,
  bookingUrlBindingMissing,
  templateUsesBookingUrl,
  withMeetingTypeBinding,
} from "@/features/campaigns/rules";
import type {
  DeliveryChannel,
  PreflightResult,
} from "@/lib/api/campaigns";

describe("channelBlocked / channelGateReasons", () => {
  it("channel_ready=false 또는 blocking_reasons가 있으면 차단", () => {
    expect(channelBlocked(null)).toBe(false);
    expect(channelBlocked({})).toBe(false);
    expect(channelBlocked({ channel_ready: true })).toBe(false);
    expect(channelBlocked({ channel_ready: false })).toBe(true);
    expect(
      channelBlocked({ blocking_reasons: ["PROVIDER_POLICY_UNVERIFIED"] })
    ).toBe(true);
  });

  it("policy_status!=allowed 또는 enabled=false 면 게이트 사유를 반환", () => {
    const base: DeliveryChannel = {
      id: "c1",
      mail_account_id: "m1",
      provider: "zoho",
      purpose: "marketing",
      name: "채널",
      policy_status: "allowed",
      enabled: true,
    };
    expect(channelGateReasons(null)).toEqual([]);
    expect(channelGateReasons(base)).toEqual([]);
    expect(
      channelGateReasons({ ...base, policy_status: "unverified" })
    ).toEqual(["PROVIDER_POLICY_UNVERIFIED"]);
    expect(channelGateReasons({ ...base, policy_status: "blocked" })).toEqual([
      "PROVIDER_POLICY_BLOCKED",
    ]);
    expect(channelGateReasons({ ...base, enabled: false })).toEqual([
      "CHANNEL_DISABLED",
    ]);
    expect(
      channelGateReasons({ ...base, policy_status: "blocked", enabled: false })
    ).toEqual(["PROVIDER_POLICY_BLOCKED", "CHANNEL_DISABLED"]);
  });
});

describe("launchActions", () => {
  it("frozen 상태는 승인 요청·취소만 가능", () => {
    const a = launchActions({ status: "frozen" });
    expect(a.requestApproval).toEqual({ visible: true, disabled: false });
    expect(a.cancel).toEqual({ visible: true, disabled: false });
    expect(a.pause).toEqual({ visible: false, disabled: true });
  });

  it("게이트 차단 시 실행으로 이어지는 행동은 disabled — 버튼은 남는다", () => {
    const a = launchActions({ status: "frozen" }, { gateBlocked: true });
    expect(a.requestApproval).toEqual({ visible: true, disabled: true });
    expect(a.pause).toEqual({ visible: false, disabled: true });
    // 취소(위험 축소)는 게이트와 무관하게 가능
    expect(a.cancel).toEqual({ visible: true, disabled: false });
  });

  it("building은 취소만, running/approved/awaiting_approval은 일시정지·취소", () => {
    const b = launchActions({ status: "building" });
    expect(b.requestApproval.visible).toBe(false);
    expect(b.pause.visible).toBe(false);
    expect(b.cancel.visible).toBe(true);

    for (const s of ["awaiting_approval", "approved", "running"]) {
      const a = launchActions({ status: s });
      expect(a.requestApproval.visible).toBe(false);
      expect(a.pause.visible).toBe(true);
      expect(a.cancel.visible).toBe(true);
    }

    // 이미 일시정지된 실행은 다시 멈출 수 없고 취소만 가능하다.
    const p = launchActions({ status: "paused" });
    expect(p.pause.visible).toBe(false);
    expect(p.cancel.visible).toBe(true);
  });

  it("completed/canceled는 아무 행동도 없다", () => {
    for (const s of ["completed", "canceled"]) {
      const a = launchActions({ status: s });
      expect(a.requestApproval.visible).toBe(false);
      expect(a.pause.visible).toBe(false);
      expect(a.cancel.visible).toBe(false);
    }
  });

  it("allowed_actions 힌트가 있으면 없는 행동을 숨긴다", () => {
    const a = launchActions(
      { status: "running" },
      { allowedActions: ["cancel"] }
    );
    expect(a.pause.visible).toBe(false);
    expect(a.cancel.visible).toBe(true);
  });
});

describe("campaignActions", () => {
  it("종결 캠페인은 새 revision 불가, 복제는 항상 가능", () => {
    const a = campaignActions({ status: "completed" });
    expect(a.newRevision.visible).toBe(false);
    expect(a.editDraft.visible).toBe(false);
    expect(a.clone.visible).toBe(true);
  });

  it("draft는 수정(새 revision) 가능", () => {
    const a = campaignActions({ status: "draft" });
    expect(a.newRevision.visible).toBe(true);
    expect(a.editDraft.visible).toBe(true);
  });

  it("allowed_actions가 없으면 숨긴다", () => {
    const a = campaignActions(
      { status: "draft" },
      { allowedActions: [] }
    );
    expect(a.newRevision.visible).toBe(false);
    expect(a.clone.visible).toBe(false);
  });
});

describe("summarizePreflight / passedRecipientIds", () => {
  const result: PreflightResult = {
    checked_at: "2026-10-09T04:00:00Z",
    global_blocking_reasons: ["PROVIDER_POLICY_UNVERIFIED"],
    recipients: [
      {
        email_address_id: "a",
        recipient_checks_passed: true,
        reason_codes: [],
      },
      {
        email_address_id: "b",
        recipient_checks_passed: false,
        reason_codes: ["RECIPIENT_SUPPRESSED", "SEND_BASIS_UNVERIFIED"],
      },
      {
        email_address_id: "c",
        recipient_checks_passed: false,
        reason_codes: [],
      },
    ],
  };

  it("통과/제외 건수와 사유별 건수를 집계한다", () => {
    const s = summarizePreflight(result);
    expect(s.passed).toBe(1);
    expect(s.excluded).toBe(2);
    expect(s.byReason.get("RECIPIENT_SUPPRESSED")).toBe(1);
    expect(s.byReason.get("SEND_BASIS_UNVERIFIED")).toBe(1);
    // 사유 코드가 없는 제외는 UNKNOWN으로 집계
    expect(s.byReason.get("UNKNOWN")).toBe(1);
    expect(s.globalBlocking).toEqual(["PROVIDER_POLICY_UNVERIFIED"]);
  });

  it("통과한 주소만 반환한다", () => {
    expect(passedRecipientIds(result)).toEqual(["a"]);
    expect(passedRecipientIds(null)).toEqual([]);
  });
});

describe("latestRevision", () => {
  it("selects the highest revision number regardless of list order", () => {
    expect(
      latestRevision([{ revision_no: 2 }, { revision_no: 5 }, { revision_no: 1 }])
    ).toEqual({ revision_no: 5 });
  });

  it("returns undefined for an empty list", () => {
    expect(latestRevision([])).toBeUndefined();
  });
});

describe("meeting booking URL binding", () => {
  it("requires booking binding only when the selected template uses its placeholder", () => {
    expect(
      templateUsesBookingUrl({
        subject: "Consultation",
        body_text: "Hello {{ contact.name }}",
        body_html: "<p>Book a meeting</p>",
      })
    ).toBe(false);
    expect(
      templateUsesBookingUrl({
        subject: "Your meeting",
        body_text: "Book: {{ meeting.booking_url }}",
        body_html: null,
      })
    ).toBe(true);
    expect(
      templateUsesBookingUrl({
        subject: "Consultation",
        body_text: "",
        body_html: '<a href="{{meeting.booking_url}}">Book</a>',
      })
    ).toBe(true);
  });

  it("blocks progression only when a selected booking template has no meeting type", () => {
    const bookingTemplate = {
      subject: "Consultation",
      body_text: "Book: {{ meeting.booking_url }}",
    };
    expect(bookingUrlBindingMissing([], "")).toBe(false);
    expect(bookingUrlBindingMissing([{ subject: "Hello", body_text: "Hi" }], "")).toBe(false);
    expect(bookingUrlBindingMissing([bookingTemplate], "")).toBe(true);
    expect(bookingUrlBindingMissing([bookingTemplate], "type-1")).toBe(false);
    expect(bookingUrlBindingMissing([bookingTemplate], "inactive-type", ["type-1"])).toBe(true);
    expect(bookingUrlBindingMissing([bookingTemplate], "type-1", ["type-1"])).toBe(false);
  });

  it("changes or clears the binding while preserving other schedule rules", () => {
    expect(
      withMeetingTypeBinding(
        { weekdays: [1, 2], meeting_type_id: "old-type" },
        "new-type"
      )
    ).toEqual({ weekdays: [1, 2], meeting_type_id: "new-type" });
    expect(
      withMeetingTypeBinding(
        { weekdays: [1, 2], meeting_type_id: "old-type" },
        ""
      )
    ).toEqual({ weekdays: [1, 2] });
  });
});
