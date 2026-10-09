import {
  Ban,
  CheckCircle2,
  Circle,
  CircleDashed,
  Clock,
  Flag,
  HelpCircle,
  Hourglass,
  Inbox,
  Info,
  Loader2,
  OctagonX,
  PauseCircle,
  Play,
  ShieldCheck,
  SkipForward,
  TriangleAlert,
  Upload,
  XCircle,
  type LucideIcon,
} from "lucide-react";
import { useTranslations } from "next-intl";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

export type StatusTone = "success" | "info" | "teal" | "amber" | "rose" | "neutral";

export interface StatusMeta {
  tone: StatusTone;
  icon: LucideIcon;
  /** 최종(더 진행되지 않는) 상태가 아니면 점선 테두리로 "비확정"을 표현한다. */
  final: boolean;
}

const M = (tone: StatusTone, icon: LucideIcon, final: boolean): StatusMeta => ({
  tone,
  icon,
  final,
});

/**
 * 도메인 상태 → 톤/아이콘 매핑.
 * 범위: 소스 §6.2 상태표 + DDL CHECK 집합(부록 A) + 작업 상태(queued/running/retry_wait/succeeded/dead/canceled).
 * 규칙: 아이콘+텍스트 병기(색상 단독 금지), unknown·confirming 은 비확정으로 명확히 표시.
 */
export const STATUS_META = {
  // ── 관계·주체 ────────────────────────────────
  prospect: M("neutral", CircleDashed, true),
  engaged: M("info", Info, false),
  qualified: M("teal", Flag, false),
  customer: M("success", CheckCircle2, true),
  new: M("info", CircleDashed, false),
  active: M("success", CheckCircle2, false),
  departed: M("neutral", Circle, true),
  inactive: M("neutral", Circle, true),
  invited: M("amber", Clock, false),
  disabled: M("neutral", Ban, true),

  // ── 주소 검증·발송 근거·정책 ──────────────────
  unknown: M("amber", HelpCircle, false),
  verified: M("success", CheckCircle2, true),
  invalid: M("rose", XCircle, true),
  risky: M("amber", TriangleAlert, false),
  pending: M("amber", Clock, false),
  valid: M("success", CheckCircle2, true),
  rejected: M("rose", XCircle, true),
  revoked: M("rose", Ban, true),
  expired: M("neutral", Clock, true),
  unverified: M("amber", HelpCircle, false),
  allowed: M("success", ShieldCheck, true),
  blocked: M("rose", Ban, true),
  needs_review: M("amber", TriangleAlert, false),
  invitation_bound: M("teal", ShieldCheck, false),
  reauth_required: M("amber", TriangleAlert, false),

  // ── 캠페인·리비전·실행 ──────────────────────
  draft: M("neutral", CircleDashed, false),
  building: M("amber", Loader2, false),
  frozen: M("teal", PauseCircle, false),
  awaiting_approval: M("amber", Hourglass, false),
  approved: M("success", CheckCircle2, true),
  paused: M("amber", PauseCircle, false),
  completed: M("success", CheckCircle2, true),
  stopped: M("neutral", Circle, true),
  suppressed: M("rose", Ban, true),

  // ── 발송 작업·제공자 이벤트·시도 ─────────────
  queued: M("amber", Clock, false),
  claimed: M("info", Play, false),
  submitting: M("info", Loader2, false),
  accepted: M("info", CheckCircle2, false),
  delivered: M("success", CheckCircle2, true),
  bounced: M("rose", XCircle, true),
  complained: M("rose", OctagonX, true),
  failed: M("rose", XCircle, true),
  started: M("info", Play, false),
  definitive_failed: M("rose", OctagonX, true),

  // ── 스레드·할 일 ────────────────────────────
  open: M("amber", CircleDashed, false),
  resolved: M("success", CheckCircle2, true),
  done: M("success", CheckCircle2, true),

  // ── 예약·슬롯·변경 제안 ─────────────────────
  held: M("amber", Hourglass, false),
  confirming: M("amber", Loader2, false),
  confirmed: M("success", CheckCircle2, true),
  canceling: M("amber", Loader2, false),
  proposed: M("info", CircleDashed, false),
  processing: M("info", Loader2, false),
  reserved: M("amber", Clock, false),
  submitted: M("info", Upload, false),
  released: M("neutral", Circle, true),

  // ── 승인 요청 ───────────────────────────────
  // pending/approved/rejected/expired/revoked 는 위 매핑 공유.

  // ── 가져오기·행·웹훅 ────────────────────────
  uploaded: M("info", Upload, false),
  validating: M("info", Loader2, false),
  ready: M("teal", Flag, false),
  committing: M("info", Loader2, false),
  duplicate: M("amber", TriangleAlert, true),
  imported: M("success", CheckCircle2, true),
  skipped: M("neutral", SkipForward, true),
  received: M("info", Inbox, false),
  processed: M("success", CheckCircle2, true),
  clean: M("success", ShieldCheck, true),

  // ── 작업 큐 상태 ────────────────────────────
  running: M("info", Loader2, false),
  retry_wait: M("amber", Clock, false),
  succeeded: M("success", CheckCircle2, true),
  dead: M("rose", OctagonX, true),

  // ── 공통 종결 ───────────────────────────────
  canceled: M("neutral", Ban, true),
} satisfies Record<string, StatusMeta>;

const TONE_VARIANT = {
  success: "success-subtle",
  info: "info-subtle",
  teal: "teal-subtle",
  amber: "amber-subtle",
  rose: "rose-subtle",
  neutral: "secondary",
} as const;

const FALLBACK_META: StatusMeta = { tone: "neutral", icon: HelpCircle, final: false };

export function statusMeta(status: string): StatusMeta {
  return (STATUS_META as Record<string, StatusMeta>)[status] ?? FALLBACK_META;
}

export interface StatusBadgeProps {
  /** §6.2·DDL 상태 문자열 (예: "queued", "confirming"). 알 수 없는 값은 neutral+? 로 렌더. */
  status: string;
  /** 라벨 직접 지정 — 기본은 messages.status.<status>. */
  label?: string;
  className?: string;
}

export function StatusBadge({ status, label, className }: StatusBadgeProps) {
  const t = useTranslations("status");
  const meta = statusMeta(status);
  const Icon = meta.icon;
  const text = label ?? (t.has(status) ? t(status) : status);

  return (
    <Badge
      variant={TONE_VARIANT[meta.tone]}
      data-status={status}
      data-tone={meta.tone}
      data-final={meta.final}
      className={cn(!meta.final && "border-dashed", className)}
    >
      <Icon aria-hidden className="shrink-0" />
      <span>{text}</span>
    </Badge>
  );
}
