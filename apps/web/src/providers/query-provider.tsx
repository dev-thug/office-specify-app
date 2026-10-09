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
            staleTime: 15_000,
            retry: shouldRetry,
            refetchOnWindowFocus: false,
          },
          mutations: {
            retry: false,
          },
        },
      })
  );

  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}
