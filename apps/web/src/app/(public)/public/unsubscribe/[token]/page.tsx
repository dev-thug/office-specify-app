import type { Metadata } from "next";
import type { ReactNode } from "react";

import { UnsubscribeConfirm } from "./unsubscribe-confirm";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/**
 * 수신 거부 — GET은 아무것도 변경하지 않는다.
 * 실제 차단은 확인 버튼의 POST에서만 일어나고, 재요청은 멱등이다.
 */
export default function PublicUnsubscribePage(): ReactNode {
  return <UnsubscribeConfirm />;
}
