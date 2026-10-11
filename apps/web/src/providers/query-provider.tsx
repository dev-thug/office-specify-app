"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";

import { ApiError } from "@/lib/api/problem";

function shouldRetry(failureCount: number, error: unknown): boolean {
  if (failureCount >= 2) return false;
  // ApiError: 문제가 없거나 재시도 불가로 표시된 오류는 재시도하지 않는다.
  if (error instanceof ApiError) {
    if (error.problem.retryable === false) return false;
    if (error.problem.status >= 400 && error.problem.status < 500) return false;
    return error.problem.retryable === true || error.problem.status >= 500;
  }
  return true;
}

export function QueryProvider({ children }: { children: ReactNode }) {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            // 화면의 데이터는 실시간에 가깝게 유지한다: 창으로 돌아오거나 네트워크가
            // 복구되면 즉시 다시 가져오고, 오래된 캐시를 오래 재사용하지 않는다.
            staleTime: 3_000,
            retry: shouldRetry,
            refetchOnWindowFocus: true,
            refetchOnReconnect: true,
          },
          mutations: {
            retry: false,
          },
        },
      })
  );

  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}
