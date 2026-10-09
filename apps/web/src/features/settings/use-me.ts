"use client";

/**
 * 현재 로그인 주체 조회 훅 — 버튼 숨김/비활성 힌트 전용이다.
 * allowed_actions·role 은 화면 힌트일 뿐 권한 보장이 아니다 (§5.2, §12.2).
 */
import { useQuery } from "@tanstack/react-query";

import { getMe, type MeData } from "@/lib/api/auth";

export function useMe() {
  return useQuery<MeData>({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    staleTime: 60_000,
  });
}

/** Admin 전용 UI(초대·grant·긴급 중지·재연결) 표시 여부. */
export function useIsAdmin(): boolean {
  const { data } = useMe();
  return data?.role === "admin";
}
