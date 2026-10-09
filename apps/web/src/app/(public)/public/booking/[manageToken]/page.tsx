import type { Metadata } from "next";
import { PublicManageScreen } from "@/features/booking";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/**
 * 예약 관리 링크 — 토큰만으로 GET 조회한다(로그인 불필요).
 * confirming 은 "확정 중"으로 표시하고 confirmed 와 구분한다. 새로고침해도 동일 상태.
 */
export default async function PublicBookingManagePage({ params }: { params: Promise<{ manageToken: string }> }) {
  const { manageToken } = await params;
  return <PublicManageScreen manageToken={manageToken} />;
}
