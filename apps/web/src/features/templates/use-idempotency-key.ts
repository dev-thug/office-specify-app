"use client";

/**
 * 논리 작업당 하나의 멱등 키를 보유하는 훅 (B.1.3, DESIGN.md "API usage").
 * 성공 또는 재시도 불가(<500, retryable !== true) 확정 응답 후에만 재발급한다 —
 * 네트워크 실패·5xx 재시도는 같은 키를 유지해 서버 중복 처리를 막는다.
 */
import { useRef } from "react";

import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

export function useIdempotencyKey() {
  const ref = useRef<string>(idempotencyKey());
  return {
    key: () => ref.current,
    /** 작업이 끝났을 때 호출 — err 가 확정 응답(성공/재시도 불가)이면 새 키로 바꾼다. */
    settle: (err?: unknown) => {
      if (!err) {
        ref.current = idempotencyKey();
        return;
      }
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        ref.current = idempotencyKey();
      }
    },
  };
}
