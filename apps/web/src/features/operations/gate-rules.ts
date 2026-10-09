/**
 * 게이트 규칙 — 정책 미허용 채널의 활성화를 UI에서도 차단한다.
 * 서버 DDL CHECK 가 최종 방어선이며 이 함수는 표시 힌트다 (§5.2, §12.2).
 */
import type { DeliveryChannel } from "@/lib/api/settings";

/**
 * enabled 토글 비활성 조건:
 * - Admin 이 아니거나,
 * - 저장이 진행 중이거나,
 * - policy_status !== "allowed" 인 채널을 새로 켜려는 경우.
 * allowed 인 채널을 끄는 것(비활성화)은 항상 허용한다.
 */
export function channelToggleDisabled(
  channel: Pick<DeliveryChannel, "policy_status" | "enabled">,
  opts: { isAdmin: boolean; pending?: boolean }
): boolean {
  if (!opts.isAdmin || opts.pending) return true;
  if (!channel.enabled && channel.policy_status !== "allowed") return true;
  return false;
}

/** 긴급 중지 확인 폼 활성 조건 — 사유 필수 + 확인 문구 정확히 일치. */
export function emergencyStopFormValid(
  reason: string,
  typed: string,
  confirmWord: string
): boolean {
  return reason.trim().length > 0 && typed === confirmWord;
}
