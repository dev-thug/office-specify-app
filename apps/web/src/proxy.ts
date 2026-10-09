import { NextRequest, NextResponse } from "next/server";

import { routing } from "@/i18n/routing";

export const SESSION_COOKIE = "outreach_session";
export const LOCALE_COOKIE = "outreach_locale";

function negotiateLocale(acceptLanguage: string | null): string {
  const candidates = (acceptLanguage ?? "")
    .split(",")
    .map((part) => part.trim().split(";")[0] ?? "")
    .map((tag) => tag.toLowerCase());
  for (const tag of candidates) {
    for (const locale of routing.locales) {
      if (tag === locale || tag.startsWith(`${locale}-`)) return locale;
    }
  }
  return routing.defaultLocale;
}

/**
 * 요청 프록시 (Next 16의 middleware 대체 컨벤션).
 * - 로케일: URL 접두사 없이 쿠키/Accept-Language로 결정하고 outreach_locale 에 보존한다.
 *   실제 locale 해석은 src/i18n/request.ts 가 같은 규칙으로 수행한다.
 * - 인증: /app/* 에 outreach_session 이 없으면 /login?next=… 로 보낸다.
 */
export default function proxy(request: NextRequest) {
  const { pathname } = request.nextUrl;

  if (pathname === "/app" || pathname.startsWith("/app/")) {
    if (!request.cookies.has(SESSION_COOKIE)) {
      const url = request.nextUrl.clone();
      const next = pathname + request.nextUrl.search;
      url.pathname = "/login";
      url.search = `?next=${encodeURIComponent(next)}`;
      const redirect = NextResponse.redirect(url);
      if (!request.cookies.has(LOCALE_COOKIE)) {
        redirect.cookies.set(LOCALE_COOKIE, negotiateLocale(request.headers.get("accept-language")), {
          path: "/",
          maxAge: 60 * 60 * 24 * 365,
          sameSite: "lax",
        });
      }
      return redirect;
    }
  }

  const response = NextResponse.next();
  if (!request.cookies.has(LOCALE_COOKIE)) {
    response.cookies.set(LOCALE_COOKIE, negotiateLocale(request.headers.get("accept-language")), {
      path: "/",
      maxAge: 60 * 60 * 24 * 365,
      sameSite: "lax",
    });
  }
  return response;
}

export const config = {
  // 명시 매처 — 정적 자산·API 프록시는 애초에 실행되지 않는다.
  // /app/* 는 세션 게이트, 나머지 페이지 경로는 로케일 쿠키 보존용.
  matcher: [
    "/",
    "/app/:path*",
    "/login",
    "/invite/:path*",
    "/public/:path*",
  ],
};
