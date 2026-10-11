import type { Metadata } from "next";
import type { ReactNode } from "react";

import { UnsubscribeConfirm } from "./unsubscribe-confirm";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/**
 * 수신 거부 — GET은 아무것도 변경하지 않는다 (§11.2: 메일 보안 스캐너의
 * 링크 조회가 상태 변경을 일으키지 않도록). 실제 차단은 확인 버튼의
 * POST /public/v1/unsubscribe/{token} 에서만 일어나고 재요청은 멱등이다.
 */
export default async function PublicUnsubscribePage({
  params,
}: {
  params: Promise<{ token: string }>;
}): Promise<ReactNode> {
  const { token } = await params;
  return <div className="mx-auto flex w-full max-w-xl flex-1 flex-col"><UnsubscribeConfirm token={token} /></div>;
}
