import type { Metadata } from "next";
import { PublicBookingScreen } from "@/features/booking";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/**
 * 공개 예약 링크 — GET은 미리보기만 한다(상태 변경 없음).
 * 슬롯·호스트 정보는 후속 태스크에서 /public/booking-links/:token 으로 로드한다.
 */
export default async function PublicBookPage({ params }: { params: Promise<{ token: string }> }) {
  const { token } = await params;
  return <PublicBookingScreen token={token} />;
}
