import { getRequestConfig } from "next-intl/server";
import { cookies, headers } from "next/headers";
import { routing, type Locale } from "./routing";

function isLocale(value: string | undefined): value is Locale {
  return (
    value !== undefined &&
    (routing.locales as readonly string[]).includes(value)
  );
}

export default getRequestConfig(async ({ requestLocale }) => {
  // 1) middleware(next-intl)가 협상한 값 우선
  let locale = await requestLocale;

  // 2) middleware 밖(정적 생성·직접 요청)에서는 쿠키 → Accept-Language → 기본값
  if (!isLocale(locale)) {
    const store = await cookies();
    const cookieLocale = store.get("outreach_locale")?.value;
    if (isLocale(cookieLocale)) {
      locale = cookieLocale;
    } else {
      const accept = ((await headers()).get("accept-language") ?? "").toLowerCase();
      locale = accept.includes("ko")
        ? "ko"
        : accept.includes("en")
          ? "en"
          : routing.defaultLocale;
    }
  }

  return {
    locale,
    messages: (await import(`../../messages/${locale}.json`)).default,
  };
});
