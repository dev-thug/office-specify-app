import { describe, expect, it } from "vitest";

import {
  canCancelOutbound,
  canRequestOutboundApproval,
  canSubmitOutbound,
  preflightDisposition,
  type OutboundMessage,
  type OutboundPreflight,
} from "../sending-state";

const outbound = (status: string): OutboundMessage => ({
  outbound_message_id: "outbound-1",
  status,
  delivery_status: "unknown",
  thread_id: null,
  approval_status: null,
  approval_valid: false,
  approval_snapshot_matches: true,
  approval_expires_at: null,
  engagement: {
    tracking_enabled: false,
    open_request_count: 0,
    click_request_count: 0,
  },
  known_replies: [],
  payload_hash: "ab".repeat(32),
  from_address: "team@example.test",
  to_address: "person@example.test",
  purpose: "correspondence",
  subject: "Re: Hello",
  body_text: "Thanks for writing.",
  version: 3,
  automatic_resubmit_allowed: false,
  next_action: status === "unknown" ? "reconcile_with_provider" : "none",
  created_at: "2026-10-09T00:00:00Z",
});

const clearPreflight: OutboundPreflight = {
  checked_at: "2026-10-09T00:00:00Z",
  is_execution_authorization: false,
  global_blocking_reasons: [],
  checks: [{ name: "recipient_basis", passed: true, code: null }],
};

describe("Inbox sending state", () => {
  it("requires every preflight check to pass before direct submit", () => {
    const failed: OutboundPreflight = {
      ...clearPreflight,
      checks: [
        { name: "recipient_basis", passed: true, code: null },
        { name: "sender_verified", passed: false, code: "SENDER_UNVERIFIED" },
      ],
    };

    expect(preflightDisposition(failed)).toEqual({
      canSubmit: false,
      canRequestApproval: false,
      failedChecks: ["SENDER_UNVERIFIED"],
    });
    expect(canSubmitOutbound(outbound("draft"), "human", failed, true)).toBe(false);
  });

  it("routes an approval-required preflight to a request instead of submit", () => {
    const required: OutboundPreflight = {
      ...clearPreflight,
      checks: [
        { name: "approval_state", passed: false, code: "APPROVAL_REQUIRED" },
        { name: "approval_hash", passed: false, code: "APPROVAL_REQUIRED" },
        { name: "approver_permission", passed: false, code: "APPROVAL_REQUIRED" },
      ],
    };

    expect(preflightDisposition(required)).toEqual({
      canSubmit: false,
      canRequestApproval: true,
      failedChecks: ["APPROVAL_REQUIRED", "APPROVAL_REQUIRED", "APPROVAL_REQUIRED"],
    });
    expect(canRequestOutboundApproval(outbound("draft"), "human", required)).toBe(
      true
    );
    expect(canSubmitOutbound(outbound("draft"), "human", required, true)).toBe(false);
  });

  it("allows only a human to submit a draft with a fully passing non-binding preflight", () => {
    expect(canSubmitOutbound(outbound("draft"), "human", clearPreflight, true)).toBe(
      true
    );
    expect(canSubmitOutbound(outbound("draft"), "agent", clearPreflight, true)).toBe(
      false
    );
    expect(canSubmitOutbound(outbound("draft"), "system", clearPreflight, true)).toBe(
      false
    );
    expect(canSubmitOutbound(outbound("draft"), "human", clearPreflight, false)).toBe(
      false
    );
  });

  it("keeps unknown outcomes lookup-only and permits cancellation only before submitting", () => {
    expect(canSubmitOutbound(outbound("unknown"), "human", clearPreflight, true)).toBe(
      false
    );
    expect(canCancelOutbound("unknown")).toBe(false);
    expect(canCancelOutbound("draft")).toBe(true);
    expect(canCancelOutbound("queued")).toBe(true);
    expect(canCancelOutbound("claimed")).toBe(true);
    expect(canCancelOutbound("submitting")).toBe(false);
  });
});
