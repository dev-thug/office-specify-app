import { defineRouting } from "next-intl/routing";

// URL에 로케일을 넣지 않는다 — 쿠키/Accept-Language 기반 (spec §5).
export const routing = defineRouting({
  locales: ["ko", "en"],
  defaultLocale: "ko",
  localePrefix: "never",
  localeCookie: {
    name: "outreach_locale",
    maxAge: 60 * 60 * 24 * 365,
  },
});

export type Locale = (typeof routing.locales)[number];
