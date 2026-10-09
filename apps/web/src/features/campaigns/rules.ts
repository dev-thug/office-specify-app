/**
 * campaigns 영역 순수 규칙 — 상태·게이트에 따른 행동 가능 여부.
 * 컴포넌트와 분리해 테스트한다. 서버가 최종 검사하며 이 결과는 UI 힌트다.
 */
import type {
  CampaignListItem,
  CampaignLaunch,
  DeliveryChannel,
  PreflightResult,
  ScheduleRules,
} from "@/lib/api/campaigns";
import type { TemplateVersion } from "@/lib/api/templates";

/** §7.3/§7.5 — 채널 미허용이면 저장·초안만 허용한다. 최상단 차단 배너 대상. */
export function channelBlocked(campaign: {
  channel_ready?: boolean;
  blocking_reasons?: string[];
} | null | undefined): boolean {
  if (!campaign) return false;
  if (campaign.channel_ready === false) return true;
  return (campaign.blocking_reasons?.length ?? 0) > 0;
}

/** 채널 행 기준 게이트 — policy_status!=allowed 또는 enabled=false 면 차단. */
export function channelGateReasons(channel: DeliveryChannel | null | undefined): string[] {
  if (!channel) return [];
  const reasons: string[] = [];
  if (channel.policy_status !== "allowed") {
    reasons.push(
      channel.policy_status === "blocked"
        ? "PROVIDER_POLICY_BLOCKED"
        : "PROVIDER_POLICY_UNVERIFIED"
    );
  }
  if (!channel.enabled) reasons.push("CHANNEL_DISABLED");
  return reasons;
}

export interface LaunchAction {
  /** 버튼을 보일지 — 권한·종결 상태로 숨긴다. */
  visible: boolean;
  /** 보이지만 누를 수 없는 상태(예: 게이트 차단). */
  disabled: boolean;
}

const VISIBLE = (disabled = false): LaunchAction => ({ visible: true, disabled });
const HIDDEN: LaunchAction = { visible: false, disabled: true };

export interface LaunchActionSet {
  pause: LaunchAction;
  cancel: LaunchAction;
  requestApproval: LaunchAction;
}

/** Revisions are commonly returned newest-first; choose by revision number, not array position. */
export function latestRevision<T extends { revision_no: number }>(
  revisions: readonly T[] | null | undefined
): T | undefined {
  if (!revisions?.length) return undefined;
  return revisions.reduce((latest, candidate) =>
    candidate.revision_no > latest.revision_no ? candidate : latest
  );
}

/**
 * launch 상태별 가능 행동 (DDL CHECK + §8.2).
 * - frozen: 승인 요청 가능 / 취소 가능 / 일시정지 불가
 * - awaiting_approval/approved/running/paused: 일시정지·취소
 * - building: 취소만 (승인 요청은 frozen 후)
 * - completed/canceled: 아무것도 없음
 * gateBlocked(공급자 게이트)면 실행으로 이어지는 행동은 disabled — 버튼은 보여서
 * 왜 안 되는지 배너와 함께 설명한다 (approve 버튼이 게이트를 우회하지 못한다).
 */
export function launchActions(
  launch: Pick<CampaignLaunch, "status"> | null | undefined,
  opts?: { gateBlocked?: boolean; allowedActions?: string[] }
): LaunchActionSet {
  const status = launch?.status;
  const gate = opts?.gateBlocked === true;
  const allowed = opts?.allowedActions;

  const allow = (name: string): boolean =>
    allowed === undefined || allowed.includes(name);

  const requestApproval: LaunchAction =
    status === "frozen" && allow("request_approval")
      ? VISIBLE(gate)
      : HIDDEN;

  const pause: LaunchAction =
    (status === "running" || status === "approved" || status === "awaiting_approval") &&
    allow("pause")
      ? VISIBLE(gate)
      : HIDDEN;

  const cancel: LaunchAction =
    status !== undefined &&
    ["building", "frozen", "awaiting_approval", "approved", "running", "paused"].includes(status) &&
    allow("cancel")
      ? VISIBLE(false)
      : HIDDEN;

  return { pause, cancel, requestApproval };
}

/** 캠페인(논리) 상태별 행동 — 복제는 항상 가능, 새 revision 은 종결 전까지. */
export function campaignActions(
  campaign: Pick<CampaignListItem, "status"> | null | undefined,
  opts?: { allowedActions?: string[] }
): { clone: LaunchAction; newRevision: LaunchAction; editDraft: LaunchAction } {
  const status = campaign?.status;
  const allowed = opts?.allowedActions;
  const allow = (name: string): boolean =>
    allowed === undefined || allowed.includes(name);

  const terminal = status === "completed" || status === "canceled";
  return {
    clone: allow("clone") ? VISIBLE(false) : HIDDEN,
    newRevision: !terminal && allow("create_revision") ? VISIBLE(false) : HIDDEN,
    editDraft: status === "draft" && allow("create_revision") ? VISIBLE(false) : HIDDEN,
  };
}

/** Detect the exact personalization placeholder in a selected immutable version. */
export function templateUsesBookingUrl(
  version: Pick<TemplateVersion, "subject" | "body_text" | "body_html"> | null | undefined
): boolean {
  if (!version) return false;
  const content = [version.subject, version.body_text, version.body_html ?? ""].join("\n");
  return /\{\{\s*meeting\.booking_url\s*\}\}/.test(content);
}

/** A meeting type is required only when a selected version uses the booking URL. */
export function bookingUrlBindingMissing(
  versions: readonly (Pick<TemplateVersion, "subject" | "body_text" | "body_html"> | null | undefined)[],
  meetingTypeId: string,
  availableMeetingTypeIds?: readonly string[]
): boolean {
  const usesBookingUrl = versions.some(templateUsesBookingUrl);
  if (!usesBookingUrl) return false;
  const selectedId = meetingTypeId.trim();
  return (
    !selectedId ||
    (availableMeetingTypeIds !== undefined && !availableMeetingTypeIds.includes(selectedId))
  );
}

/** Set or clear only the meeting binding while retaining the rest of schedule rules. */
export function withMeetingTypeBinding(
  rules: ScheduleRules | undefined,
  meetingTypeId: string
): ScheduleRules {
  const otherRules = { ...(rules ?? {}) };
  delete otherRules.meeting_type_id;
  const selected = meetingTypeId.trim();
  return selected ? { ...otherRules, meeting_type_id: selected } : otherRules;
}

export interface PreflightSummary {
  passed: number;
  excluded: number;
  byReason: Map<string, number>;
  globalBlocking: string[];
  checkedAt?: string;
}

/** preflight 결과를 사유별 건수로 요약한다 (§7.3 "제외 사유 건수"). */
export function summarizePreflight(
  result: PreflightResult | null | undefined
): PreflightSummary {
  const summary: PreflightSummary = {
    passed: 0,
    excluded: 0,
    byReason: new Map(),
    globalBlocking: result?.global_blocking_reasons ?? [],
    checkedAt: result?.checked_at,
  };
  for (const r of result?.recipients ?? []) {
    if (r.recipient_checks_passed) {
      summary.passed += 1;
    } else {
      summary.excluded += 1;
      for (const code of r.reason_codes) {
        summary.byReason.set(code, (summary.byReason.get(code) ?? 0) + 1);
      }
      if (r.reason_codes.length === 0) {
        summary.byReason.set("UNKNOWN", (summary.byReason.get("UNKNOWN") ?? 0) + 1);
      }
    }
  }
  return summary;
}

/** preflight 통과 주소만 모은다 — 동결 대상. */
export function passedRecipientIds(result: PreflightResult | null | undefined): string[] {
  return (result?.recipients ?? [])
    .filter((r) => r.recipient_checks_passed)
    .map((r) => r.email_address_id);
}
