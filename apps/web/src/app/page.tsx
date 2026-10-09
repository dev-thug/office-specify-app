import { redirect } from "next/navigation";

/** 루트는 항상 앱 대시보드로 보낸다 (미인증은 미들웨어가 /login 으로). */
export default function RootPage() {
  redirect("/app/dashboard");
}
