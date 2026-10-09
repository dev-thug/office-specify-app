import { describe, expect, it } from "vitest";

import type {
  EmailMessage,
  EmailThreadDetailData,
  EmailThreadSummary,
} from "@/lib/api/inbox";
import {
  canAction,
  canComposeInThread,
  computeBaseline,
  detectCollision,
  downloadableAttachments,
  formatBytes,
  inboxErrorKey,
  inferredReplyTo,
  isThreadUnread,
  MESSAGE_IFRAME_CSP,
  needsReconnect,
  nextPendingStatus,
  nextResolveStatus,
  participantAddresses,
  reconnectAccounts,
  replyBlockReason,
  sortMessages,
  suppressionCandidates,
  suppressionHref,
  visibleMessages,
  wrapSanitizedHtml,
} from "../thread-state";

function thread(partial: Partial<EmailThreadSummary>): EmailThreadSummary {
  return {
    id: "t1",
    mail_account_id: "ma1",
    contact_id: null,
    assigned_principal_id: null,
    status: "open",
    last_inbound_message_id: null,
    version: 1,
    updated_at: "2026-10-12T01:00:00Z",
    created_at: "2026-10-12T00:00:00Z",
    ...partial,
  };
}

function message(partial: Partial<EmailMessage>): EmailMessage {
  return {
    id: "m0",
    direction: "inbound",
    subject: null,
    body_text: null,
    body_html_sanitized: null,
    message_at: "2026-10-12T01:00:00Z",
    classification: "unknown",
    ...partial,
  };
}

function detail(
  messages: EmailMessage[],
  partial: Partial<EmailThreadDetailData["thread"]> = {}
): EmailThreadDetailData {
  return {
    thread: {
      id: "t1",
      mail_account_id: "ma1",
      contact_id: null,
      assigned_principal_id: null,
      last_inbound_message_id: null,
      status: "open",
      version: 1,
      updated_at: "2026-10-12T02:00:00Z",
      created_at: "2026-10-12T00:00:00Z",
      ...partial,
    },
    messages,
  };
}

describe("isThreadUnread", () => {
  it("unread 플래그가 있으면 그 값을 쓴다", () => {
    expect(isThreadUnread(thread({ unread: true }))).toBe(true);
    expect(isThreadUnread(thread({ unread: false }))).toBe(false);
  });
  it("플래그가 없으면 수신/읽음 시각으로 판별한다", () => {
    expect(
      isThreadUnread(
        thread({ last_inbound_at: "2026-10-12T02:00:00Z", last_read_at: null })
      )
    ).toBe(true);
    expect(
      isThreadUnread(
        thread({
          last_inbound_at: "2026-10-12T02:00:00Z",
          last_read_at: "2026-10-12T03:00:00Z",
        })
      )
    ).toBe(false);
    expect(isThreadUnread(thread({}))).toBe(false);
  });
});

describe("needsReconnect / reconnectAccounts", () => {
  it("reauth_required·disabled 만 재연결 대상이다", () => {
    expect(needsReconnect({ status: "reauth_required" })).toBe(true);
    expect(needsReconnect({ status: "disabled" })).toBe(true);
    expect(needsReconnect({ status: "active" })).toBe(false);
    expect(needsReconnect({ status: "pending" })).toBe(false);
    expect(needsReconnect(null)).toBe(false);
    expect(
      reconnectAccounts([
        { id: "1", name: "a", primary_address: "a@x", status: "active" },
        { id: "2", name: "b", primary_address: "b@x", status: "reauth_required" },
        { id: "3", name: "c", primary_address: "c@x", status: "disabled" },
      ]).map((a) => a.id)
    ).toEqual(["2", "3"]);
  });
});

describe("canAction", () => {
  it("allowed_actions 없음 → 표시(서버가 검사), 있음 → 멤버십으로 판정", () => {
    expect(canAction(undefined, "assign")).toBe(true);
    expect(canAction(["assign"], "assign")).toBe(true);
    expect(canAction(["read"], "assign")).toBe(false);
    expect(canAction([], "assign")).toBe(false);
  });
});

describe("canComposeInThread", () => {
  it("uses mailbox allowed_actions when the thread response omits action hints", () => {
    expect(canComposeInThread(undefined, ["mail.read", "mail.compose"])).toBe(true);
    expect(canComposeInThread(undefined, ["mail.read"])).toBe(false);
    expect(canComposeInThread(undefined, [])).toBe(false);
    expect(canComposeInThread(undefined, undefined)).toBe(true);
  });

  it("uses explicit per-thread actions when present", () => {
    expect(canComposeInThread([], ["mail.compose"])).toBe(false);
    expect(canComposeInThread(["reply"], [])).toBe(true);
    expect(canComposeInThread(["compose"], undefined)).toBe(true);
  });
});

describe("sortMessages / visibleMessages", () => {
  it("message_at 오름차순으로 정렬하고 baseline 이후분을 숨긴다", () => {
    const m1 = message({ id: "m1", message_at: "2026-10-12T03:00:00Z" });
    const m2 = message({ id: "m2", message_at: "2026-10-12T01:00:00Z" });
    const m3 = message({ id: "m3", message_at: "2026-10-12T02:00:00Z" });
    const sorted = sortMessages([m1, m2, m3]);
    expect(sorted.map((m) => m.id)).toEqual(["m2", "m3", "m1"]);

    const d = detail([m1, m2]);
    const baseline = computeBaseline(d); // 2개 확인
    // 폴링으로 m1(03:00)이 늦게 도착한 경우처럼 baseline 이후분은 숨겨진다 —
    // 정렬 후 앞의 2개(m2, m3)만 표시하고 최신 m1 은 숨긴다.
    expect(
      visibleMessages([m1, m2, m3], baseline).map((m) => m.id)
    ).toEqual(["m2", "m3"]);
    expect(visibleMessages([m1, m2, m3], null)).toHaveLength(3);
  });
});

describe("detectCollision", () => {
  const inbound = (id: string) =>
    message({ id, direction: "inbound" });
  const outbound = (id: string) =>
    message({ id, direction: "outbound" });

  it("새 수신 도착 → new_inbound", () => {
    const d1 = detail([inbound("a")], { last_inbound_message_id: "a" });
    const base = computeBaseline(d1);
    const d2 = detail([inbound("a"), inbound("b")], {
      last_inbound_message_id: "b",
      version: 2,
    });
    expect(detectCollision(base, d2)).toBe("new_inbound");
  });

  it("다른 구성원의 발신 메시지 추가 → teammate_reply", () => {
    const d1 = detail([inbound("a")], { last_inbound_message_id: "a" });
    const base = computeBaseline(d1);
    const d2 = detail([inbound("a"), outbound("x")], {
      last_inbound_message_id: "a",
      version: 2,
    });
    expect(detectCollision(base, d2)).toBe("teammate_reply");
  });

  it("버전만 변경(배정·상태) → changed", () => {
    const d1 = detail([inbound("a")], { last_inbound_message_id: "a" });
    const base = computeBaseline(d1);
    const d2 = detail([inbound("a")], {
      last_inbound_message_id: "a",
      version: 2,
      status: "pending",
    });
    expect(detectCollision(base, d2)).toBe("changed");
  });

  it("변경 없음 → null", () => {
    const d1 = detail([inbound("a")], { last_inbound_message_id: "a" });
    expect(detectCollision(computeBaseline(d1), d1)).toBeNull();
  });
});

describe("participantAddresses / inferredReplyTo", () => {
  it("from·reply_to 주소를 추출하고 마지막 수신 보낸 사람을 추정한다", () => {
    const m = message({
      id: "m1",
      participants: [
        { role: "from", address: "sender@x.dev", display_name: "Kim" },
        { role: "to", address: "team@y.dev", display_name: null },
        { role: "reply_to", address: "reply@x.dev", display_name: null },
      ],
    });
    expect(participantAddresses(m, "from")).toEqual(["sender@x.dev"]);
    // reply_to 가 있으면 from 보다 우선한다
    expect(inferredReplyTo([m])).toEqual({
      address: "reply@x.dev",
      messageId: "m1",
    });
    expect(
      inferredReplyTo([message({ id: "m2", direction: "outbound" })])
    ).toBeNull();
  });
});

describe("replyBlockReason", () => {
  const base = {
    mailboxNeedsReconnect: false,
    collision: null,
    canCompose: true,
    hasInbound: true,
  } as const;
  it("우선순위: reconnect > collision > forbidden > no_inbound", () => {
    expect(
      replyBlockReason({ ...base, mailboxNeedsReconnect: true, collision: "new_inbound" })
    ).toBe("reconnect");
    expect(replyBlockReason({ ...base, collision: "teammate_reply" })).toBe(
      "collision"
    );
    expect(replyBlockReason({ ...base, canCompose: false })).toBe("forbidden");
    expect(replyBlockReason({ ...base, hasInbound: false })).toBe("no_inbound");
    expect(replyBlockReason(base)).toBeNull();
  });
});

describe("wrapSanitizedHtml / MESSAGE_IFRAME_CSP", () => {
  it("CSP meta 로 원격 리소스·스크립트를 차단한 문서를 만든다", () => {
    const html = wrapSanitizedHtml("<p>안녕</p>");
    expect(html).toContain("Content-Security-Policy");
    expect(html).toContain("<p>안녕</p>");
    expect(MESSAGE_IFRAME_CSP).toContain("default-src 'none'");
    // 원격 추적 이미지 차단 — data:/cid: 만 허용
    expect(MESSAGE_IFRAME_CSP).toContain("img-src data: cid:");
    expect(MESSAGE_IFRAME_CSP).not.toContain("https:");
    expect(MESSAGE_IFRAME_CSP).not.toContain("http:");
  });
});

describe("첨부·크기", () => {
  it("인라인 첨부는 다운로드 목록에서 제외한다", () => {
    const m = message({
      attachments: [
        { id: "a1", filename: "a.pdf", is_inline: false },
        { id: "a2", filename: "logo.png", is_inline: true },
      ],
    });
    expect(downloadableAttachments(m).map((a) => a.id)).toEqual(["a1"]);
  });
  it("formatBytes", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2 KB");
    expect(formatBytes(null)).toBeNull();
    expect(formatBytes(-1)).toBeNull();
  });
});

describe("suppressionCandidates / suppressionHref", () => {
  it("수신 from 주소를 중복 없이 모은다", () => {
    const msgs = [
      message({
        id: "1",
        participants: [{ role: "from", address: "a@x.dev", display_name: null }],
      }),
      message({
        id: "2",
        participants: [{ role: "from", address: "a@x.dev", display_name: null }],
      }),
      message({
        id: "3",
        direction: "outbound",
        participants: [{ role: "from", address: "us@y.dev", display_name: null }],
      }),
      message({
        id: "4",
        participants: [{ role: "from", address: "b@x.dev", display_name: null }],
      }),
    ];
    expect(suppressionCandidates(msgs)).toEqual(["a@x.dev", "b@x.dev"]);
    expect(suppressionHref("a@x.dev")).toBe(
      "/app/compliance/suppressions?address=a%40x.dev"
    );
  });
});

describe("상태 패치 대상", () => {
  it("resolve / pending 토글", () => {
    expect(nextResolveStatus("open")).toBe("resolved");
    expect(nextResolveStatus("pending")).toBe("resolved");
    expect(nextResolveStatus("resolved")).toBe("open");
    expect(nextPendingStatus("open")).toBe("pending");
    expect(nextPendingStatus("pending")).toBe("open");
  });
});

describe("inboxErrorKey — B.5 안정 코드 매핑", () => {
  it.each([
    ["REPLY_CONTEXT_CHANGED", "replyContextChanged"],
    ["REPLY_ALREADY_SUBMITTED", "replyAlreadySubmitted"],
    ["MAILBOX_ACCESS_DENIED", "mailboxDenied"],
    ["RECIPIENT_SUPPRESSED", "recipientSuppressed"],
    ["WORKSPACE_EMERGENCY_STOP", "emergencyStop"],
    ["VERSION_MISMATCH", "versionMismatch"],
    ["INTEGRATION_REAUTH_REQUIRED", "reauthRequired"],
    ["RATE_LIMITED", "rateLimited"],
  ])("%s → %s", (code, key) => {
    expect(inboxErrorKey({ status: 409, code })).toBe(key);
  });
  it("매핑 없는 코드 — retryable=true 면 재시도 키, 아니면 null", () => {
    expect(inboxErrorKey({ status: 500, code: "INTERNAL", retryable: true })).toBe(
      "retryable"
    );
    expect(
      inboxErrorKey({ status: 500, code: "INTERNAL", retryable: false })
    ).toBeNull();
  });
});
