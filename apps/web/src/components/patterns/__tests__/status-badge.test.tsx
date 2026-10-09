import { render, screen } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement } from "react";
import { describe, expect, it } from "vitest";

import { STATUS_META, StatusBadge, statusMeta } from "@/components/patterns/status-badge";
import en from "../../../../messages/en.json";

const R = (ui: ReactElement) =>
  render(
    <NextIntlClientProvider locale="en" messages={en}>
      {ui}
    </NextIntlClientProvider>
  );

describe("STATUS_META coverage", () => {
  // 소스 §6.2 상태표 + DDL CHECK 집합 — 도메인별 대표 목록.
  const REQUIRED = [
    // 주체/멤버·메일 계정
    "active", "disabled", "invited", "pending", "reauth_required", "verified",
    // 정책·발송 근거
    "unverified", "allowed", "blocked", "needs_review", "valid", "rejected", "revoked", "expired",
    // 담당자/주소
    "new", "departed", "inactive", "unknown", "invalid", "risky",
    // 캠페인·리비전
    "draft", "awaiting_approval", "paused", "completed", "canceled",
    "building", "frozen", "approved", "running",
    // 등록
    "stopped", "suppressed",
    // 스레드·발송·제공자
    "open", "resolved", "queued", "claimed", "submitting", "accepted",
    "failed", "delivered", "bounced", "complained",
    // 예약
    "held", "confirming", "confirmed", "canceling", "proposed",
    "processing", "released",
    // 승인·할 일·첨부
    "done", "clean",
    // 가져오기·웹훅·작업
    "uploaded", "validating", "ready", "committing", "duplicate",
    "imported", "skipped", "received", "processed",
    "retry_wait", "completed", "dead",
    // 에이전트·리서치
    "started", "definitive_failed", "reserved", "submitted",
    "discarded",
  ];

  it.each(REQUIRED)("maps %s to a tone, icon and finality", (status) => {
    const meta = STATUS_META[status as keyof typeof STATUS_META];
    expect(meta, `missing STATUS_META entry for ${status}`).toBeDefined();
    expect(statusMeta(status)).toBe(meta);
    expect(["success", "info", "teal", "amber", "rose", "neutral"]).toContain(meta.tone);
  });
});

describe("StatusBadge", () => {
  it("renders icon + translated text, never color alone", () => {
    R(<StatusBadge status="awaiting_approval" />);
    const badge = screen.getByText("Awaiting approval").closest("[data-slot='badge']");
    expect(badge).toBeInTheDocument();
    expect(badge?.querySelector("svg")).toBeTruthy();
    expect(badge).toHaveAttribute("data-tone", "amber");
  });

  it("marks unknown and confirming as clearly non-final", () => {
    R(
      <>
        <StatusBadge status="unknown" />
        <StatusBadge status="confirming" />
      </>
    );
    for (const s of ["unknown", "confirming"]) {
      expect(statusMeta(s).final).toBe(false);
      const badge = screen.getByText(en.status[s as keyof typeof en.status]).closest("[data-slot='badge']");
      expect(badge).toHaveAttribute("data-final", "false");
      expect(badge?.className).toContain("border-dashed");
    }
  });

  it("falls back to a neutral badge for unmapped statuses", () => {
    R(<StatusBadge status="something_new" />);
    const badge = screen.getByText("something_new").closest("[data-slot='badge']");
    expect(badge).toHaveAttribute("data-tone", "neutral");
    expect(badge?.querySelector("svg")).toBeTruthy();
  });

  it("accepts an explicit label override", () => {
    R(<StatusBadge status="dead" label="실패한 작업" />);
    expect(screen.getByText("실패한 작업")).toBeInTheDocument();
  });
});
