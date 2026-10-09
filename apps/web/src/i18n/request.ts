import { getRequestConfig } from "next-intl/server";
import { cookies, headers } from "next/headers";
import { NAMESPACES, type Namespace } from "./namespaces";
import { routing, type Locale } from "./routing";

/** messages/<locale>/<namespace>.json 전부를 읽어 { <ns>: {...} } 형태로 합친다. */
async function loadMessages(
  locale: Locale
): Promise<Record<Namespace, unknown>> {
  const parts = await Promise.all(
    NAMESPACES.map(async (ns) => {
      const mod = (await import(`../../messages/${locale}/${ns}.json`)) as {
        default: unknown;
      };
      return [ns, mod.default] as const;
    })
  );
  return Object.fromEntries(parts) as Record<Namespace, unknown>;
}

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

  // 위 분기 이후 런타임상 항상 유효하지만, string 으로 넓어진 타입을 다시 좁힌다.
  const resolved: Locale = isLocale(locale) ? locale : routing.defaultLocale;
  return {
    locale: resolved,
    messages: await loadMessages(resolved),
  };
});
