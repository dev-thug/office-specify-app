/**
 * 작업 행동 규칙 (UI13 + §16.4).
 * - unknown 결과는 절대 "재시도/재발송" 버튼을 노출하지 않는다 — 대조(조사)만.
 *   서버가 retryable=true 를 실수로 내려도 이 규칙이 우선한다.
 * - 수동 재시도는 서버의 retryable 플래그가 true 이고 안전한 종결 상태일 때만.
 *   running/queued/retry_wait 는 워커가 자체 정책으로 처리하므로 수동 개입 없음.
 */
import type { Job } from "@/lib/api/operations";

export type JobAction = "retry" | "investigate" | "none";

/** 수동 재시도 요청을 허용하는 종결 상태 — 완료·불명·진행 상태는 제외. */
export const MANUAL_RETRY_STATES: ReadonlySet<string> = new Set([
  "dead",
  "canceled",
]);

export function jobPrimaryAction(
  job: Pick<Job, "state"> & { retryable?: boolean }
): JobAction {
  if (job.state === "unknown") return "investigate";
  if (job.retryable === true && MANUAL_RETRY_STATES.has(job.state)) {
    return "retry";
  }
  return "none";
}
