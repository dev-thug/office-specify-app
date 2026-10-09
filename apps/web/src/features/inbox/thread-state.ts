/**
 * 공동 Inbox 순수 상태 로직 — 컴포넌트·테스트가 공유하는 판별 함수 모음.
 * (UI05: 배정·읽음·해결, 충돌 감지, 접수≠발송완료, 재연결 경고)
 */
import type {
  EmailMessage,
  EmailThreadDetailData,
  EmailThreadSummary,
  MailAccount,
  MessageParticipant,
  ThreadStatus,
} from "@/lib/api/inbox";

// ─── 스레드 목록 ──────────────────────────────────────────

/** 본인 기준 안읽음 여부 — 응답의 unread 플래그 우선, 없으면 시각 비교 폴백. */
export function isThreadUnread(thread: EmailThreadSummary): boolean {
  if (typeof thread.unread === "boolean") return thread.unread;
  if (thread.last_inbound_at) {
    if (!thread.last_read_at) return true;
    return thread.last_inbound_at > thread.last_read_at;
  }
  return false;
}

// ─── 메일함 연결 건강 ─────────────────────────────────────

/** 재연결이 필요한 상태 — 재인증 요구·비활성 메일함은 동기가 멈춘다. */
export function needsReconnect(
  account: { status?: MailAccount["status"] } | null | undefined
): boolean {
  return account?.status === "reauth_required" || account?.status === "disabled";
}

/** 메일함 목록 중 재연결 필요 항목만 골라 배너에 쓴다. */
export function reconnectAccounts(accounts: MailAccount[]): MailAccount[] {
  return accounts.filter(needsReconnect);
}

// ─── 권한 힌트 ────────────────────────────────────────────

/**
 * allowed_actions 는 UI 힌트다 (§12.2 — 서버가 최종 검사).
 * 목록이 없으면(구형 응답) 일단 노출하고 403 을 지역화 오류로 처리한다.
 */
export function canAction(
  allowedActions: string[] | undefined,
  action: string
): boolean {
  if (allowedActions === undefined) return true;
  return allowedActions.includes(action);
}

/**
 * Thread-detail hints may be absent in B.2 responses; in that case use the
 * caller's current mailbox action list (`mail.compose`) when it is available.
 */
export function canComposeInThread(
  threadAllowedActions: string[] | undefined,
  mailboxAllowedActions: string[] | undefined
): boolean {
  if (threadAllowedActions !== undefined) {
    return (
      threadAllowedActions.includes("reply") ||
      threadAllowedActions.includes("compose")
    );
  }
  if (mailboxAllowedActions !== undefined) {
    return mailboxAllowedActions.includes("mail.compose");
  }
  return true;
}

// ─── 메시지 정렬·표시 ─────────────────────────────────────

/** 메시지를 message_at 오름차순(오래된 → 최신)으로 정렬한 사본. */
export function sortMessages(messages: EmailMessage[]): EmailMessage[] {
  return [...messages].sort((a, b) => {
    const at = a.message_at;
    const bt = b.message_at;
    if (at === bt) return a.id.localeCompare(b.id);
    return at < bt ? -1 : 1;
  });
}

// ─── 충돌 감지 (§9.3, UI05) ───────────────────────────────

/** 사용자가 마지막으로 확인(ack)한 스레드 상태의 스냅샷. */
export interface ThreadBaseline {
  lastInboundId: string | null;
  inboundCount: number;
  outboundCount: number;
  messageCount: number;
  version: number;
}

export function computeBaseline(detail: EmailThreadDetailData): ThreadBaseline {
  const messages = detail.messages ?? [];
  return {
    lastInboundId: detail.thread.last_inbound_message_id,
    inboundCount: messages.filter((m) => m.direction === "inbound").length,
    outboundCount: messages.filter((m) => m.direction === "outbound").length,
    messageCount: messages.length,
    version: detail.thread.version,
  };
}

export type CollisionKind = "new_inbound" | "teammate_reply" | "changed";

/**
 * baseline 이후 폴링된 최신 데이터를 비교해 충돌 종류를 판별한다.
 * - 새 수신 메일 도착 → "new_inbound" (회신 전 재검토 필요 — §9.3)
 * - 다른 구성원의 발신 메일 추가 → "teammate_reply"
 * - 그 외 버전 변경 → "changed"
 */
export function detectCollision(
  baseline: ThreadBaseline,
  current: EmailThreadDetailData
): CollisionKind | null {
  if (current.thread.last_inbound_message_id !== baseline.lastInboundId) {
    return "new_inbound";
  }
  const messages = current.messages ?? [];
  const inbound = messages.filter((m) => m.direction === "inbound").length;
  const outbound = messages.filter((m) => m.direction === "outbound").length;
  if (inbound > baseline.inboundCount) return "new_inbound";
  if (outbound > baseline.outboundCount) return "teammate_reply";
  if (current.thread.version !== baseline.version) return "changed";
  return null;
}

/**
 * baseline 이후 도착한 메시지는 "새로고침" 전까지 화면에 숨긴다 —
 * 사용자가 확인한 내용과 화면 표시가 항상 일치하게 한다.
 */
export function visibleMessages(
  messages: EmailMessage[] | undefined,
  baseline: ThreadBaseline | null
): EmailMessage[] {
  const sorted = sortMessages(messages ?? []);
  if (!baseline) return sorted;
  return sorted.slice(0, baseline.messageCount);
}

// ─── 회신 ────────────────────────────────────────────────

/** 메시지에서 특정 role 의 참여자 주소 목록 — role 인자 순서대로 우선 배치한다. */
export function participantAddresses(
  message: EmailMessage,
  ...roles: MessageParticipant["role"][]
): string[] {
  const participants = message.participants ?? [];
  const out: string[] = [];
  for (const role of roles) {
    for (const p of participants) {
      if (p.role === role) out.push(p.address);
    }
  }
  return out;
}

/** 회신 수신자 추정 — 마지막 수신 메시지의 보낸 사람 (서버가 최종 확인). */
export function inferredReplyTo(
  messages: EmailMessage[]
): { address: string; messageId: string } | null {
  const inbound = sortMessages(messages)
    .filter((m) => m.direction === "inbound")
    .pop();
  if (!inbound) return null;
  const address =
    participantAddresses(inbound, "reply_to", "from")[0] ?? null;
  if (!address) return null;
  return { address, messageId: inbound.id };
}

export type ReplyBlockReason = "reconnect" | "collision" | "forbidden" | "no_inbound";

/** 회신 초안 작성을 막는 이유 — null 이면 작성 가능. */
export function replyBlockReason(input: {
  mailboxNeedsReconnect: boolean;
  collision: CollisionKind | null;
  canCompose: boolean;
  hasInbound: boolean;
}): ReplyBlockReason | null {
  if (input.mailboxNeedsReconnect) return "reconnect";
  if (input.collision !== null) return "collision";
  if (!input.canCompose) return "forbidden";
  if (!input.hasInbound) return "no_inbound";
  return null;
}

// ─── 메시지 본문 sandbox ──────────────────────────────────

/**
 * 정제된 HTML 을 iframe srcDoc 에 싣기 전 CSP meta 를 감싼다 —
 * 스크립트·폼·원격 리소스(추적 이미지 포함)를 차단하고 data:/cid: 이미지만
 * 허용한다. 서버 정제를 신뢰하되 클라이언트 측 방어를 겹친다 (§15.1).
 */
export const MESSAGE_IFRAME_CSP =
  "default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'; font-src data:;";

export function wrapSanitizedHtml(sanitizedHtml: string): string {
  return [
    "<!doctype html><html><head>",
    `<meta http-equiv="Content-Security-Policy" content="${MESSAGE_IFRAME_CSP}">`,
    '<meta name="referrer" content="no-referrer">',
    "<style>",
    // 시스템 색 이름만 사용 — 팔레트 상수 없이 사용자 테마에 맞춘다.
    "body{margin:0;padding:12px;font:13px/1.6 system-ui,sans-serif;color:CanvasText;background:Canvas;word-break:break-word}",
    "a{color:LinkText}",
    "img{max-width:100%;height:auto}",
    "</style></head><body>",
    sanitizedHtml,
    "</body></html>",
  ].join("");
}

// ─── 첨부 ────────────────────────────────────────────────

/** 다운로드 목록에는 인라인이 아닌 첨부만 노출한다. */
export function downloadableAttachments(
  message: EmailMessage
): NonNullable<EmailMessage["attachments"]> {
  return (message.attachments ?? []).filter((a) => !a.is_inline);
}

const BYTES_UNITS = ["B", "KB", "MB", "GB"] as const;

export function formatBytes(size: number | null | undefined): string | null {
  if (typeof size !== "number" || !Number.isFinite(size) || size < 0) {
    return null;
  }
  let value = size;
  let unit = 0;
  while (value >= 1024 && unit < BYTES_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const rounded = unit === 0 ? Math.round(value) : Math.round(value * 10) / 10;
  return `${rounded} ${BYTES_UNITS[unit]}`;
}

// ─── 수신 거부 등록 진입점 ────────────────────────────────

/** 스레드의 수신 보낸 사람 주소들 — 차단 등록 후보 (중복 제거, 입력 순서 유지). */
export function suppressionCandidates(
  messages: EmailMessage[] | undefined
): string[] {
  const seen = new Set<string>();
  for (const message of sortMessages(messages ?? [])) {
    if (message.direction !== "inbound") continue;
    for (const address of participantAddresses(message, "from")) {
      seen.add(address);
    }
  }
  return [...seen];
}

export function suppressionHref(address: string): string {
  return `/app/compliance/suppressions?address=${encodeURIComponent(address)}`;
}

// ─── 분류 라벨 키 ─────────────────────────────────────────

/** 분류 검토가 의미 있는 메시지 — 수신 메시지만 검토한다. */
export function isReviewable(message: EmailMessage): boolean {
  return message.direction === "inbound";
}

// ─── API 오류 → 로컬라이즈 키 ─────────────────────────────

export interface ProblemLike {
  status: number;
  code: string;
  retryable?: boolean;
}

/**
 * B.5 안정 코드 → inbox.errors.<key> 매핑. null 이면 일반 오류 표시를 쓴다.
 * retryable=true 문제는 재시도 affordance 를 함께 보여준다 (파형 규칙).
 */
export function inboxErrorKey(problem: ProblemLike): string | null {
  switch (problem.code) {
    case "MAILBOX_ACCESS_DENIED":
      return "mailboxDenied";
    case "AGENT_DIRECT_SEND_FORBIDDEN":
      return "agentSendForbidden";
    case "REPLY_CONTEXT_CHANGED":
      return "replyContextChanged";
    case "REPLY_ALREADY_SUBMITTED":
      return "replyAlreadySubmitted";
    case "RECIPIENT_SUPPRESSED":
      return "recipientSuppressed";
    case "WORKSPACE_EMERGENCY_STOP":
      return "emergencyStop";
    case "VERSION_MISMATCH":
      return "versionMismatch";
    case "VERSION_PRECONDITION_REQUIRED":
      return "versionPrecondition";
    case "IDEMPOTENCY_KEY_REUSED":
      return "idempotencyReused";
    case "REQUEST_IN_PROGRESS":
      return "requestInProgress";
    case "INTEGRATION_REAUTH_REQUIRED":
      return "reauthRequired";
    case "RATE_LIMITED":
      return "rateLimited";
    default:
      return problem.retryable === true ? "retryable" : null;
  }
}

// ─── 상태 패치 대상 ───────────────────────────────────────

/** 해결 버튼이 다음 상태를 결정한다 — resolved 면 open, 아니면 resolved. */
export function nextResolveStatus(current: ThreadStatus): ThreadStatus {
  return current === "resolved" ? "open" : "resolved";
}

export function nextPendingStatus(current: ThreadStatus): ThreadStatus {
  return current === "pending" ? "open" : "pending";
}
