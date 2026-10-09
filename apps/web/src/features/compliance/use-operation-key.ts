"use client";

import { useCallback, useState } from "react";

import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

export interface OperationKey {
  /** 현재 논리 작업의 멱등 키 — 렌더 시점 스냅샷. */
  key: string;
  /** 성공 또는 확정된 종료 후 호출 — 다음 논리 작업은 새 키를 쓴다. */
  settle: (error: unknown) => void;
}

/**
 * 논리 작업당 하나의 Idempotency-Key 를 보유한다 (DESIGN.md "API usage",
 * B.1.3 "네트워크 재시도 시 새 키를 만들지 않는다").
 *
 * - 마운트(다이얼로그 오픈) 시 발급.
 * - 성공, 또는 retryable 아닌 <500 응답 후에만 재발급한다.
 * - 네트워크 실패·5xx·retryable 오류는 같은 키로 재시도한다.
 */
export function useOperationKey(): OperationKey {
  const [key, setKey] = useState(idempotencyKey);

  const rotate = useCallback(() => {
    setKey(idempotencyKey());
  }, []);

  const settle = useCallback(
    (error: unknown) => {
      if (error == null) {
        rotate();
        return;
      }
      // 재시도 가능한 실패(네트워크·5xx·retryable)는 키를 유지한다.
      if (
        error instanceof ApiError &&
        (error.problem.retryable === true || error.problem.status >= 500)
      ) {
        return;
      }
      rotate();
    },
    [rotate]
  );

  return { key, settle };
}
