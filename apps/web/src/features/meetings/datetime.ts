/**
 * 시간대 유틸 — 외부 날짜 라이브러리 없이 Intl 만으로 DST 안전을 만든다.
 *
 * 원칙 (§10.2, §7.6):
 * - 슬롯·예약은 UTC 인스턴트다. 표시는 항상 선택된 IANA timezone 에서 한다.
 * - "로컬 날짜"는 {y,m,d} 튜플로 다룬다 — 튜플의 달력 연산(±n일, 요일)은
 *   DST와 무관하게 안전하다.
 * - 로컬 시각 → UTC 변환은 오프셋을 반복 추정해 수렴시킨다. 존재하지 않는
 *   현지 시각(봄철 DST)은 앞으로 밀린 인스턴트에 놓인다.
 */

export interface LocalDate {
  y: number;
  m: number; // 1–12
  d: number; // 1–31
}

export interface ZonedParts extends LocalDate {
  hh: number;
  mm: number;
  ss: number;
}

export function detectTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export function isValidTimezone(tz: string): boolean {
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: tz });
    return true;
  } catch {
    return false;
  }
}

/** 공개/편집 UI에서 고르는 대표 IANA 시간대 — 감지된 시간대는 별도로 맨 위에 둔다. */
export const COMMON_TIMEZONES: readonly string[] = [
  "Asia/Seoul",
  "Asia/Tokyo",
  "Asia/Shanghai",
  "Asia/Singapore",
  "Asia/Hong_Kong",
  "Asia/Taipei",
  "Asia/Jakarta",
  "Asia/Bangkok",
  "Asia/Ho_Chi_Minh",
  "Asia/Kolkata",
  "Asia/Dubai",
  "Australia/Sydney",
  "Australia/Melbourne",
  "Pacific/Auckland",
  "Pacific/Fiji",
  "Europe/London",
  "Europe/Dublin",
  "Europe/Paris",
  "Europe/Berlin",
  "Europe/Madrid",
  "Europe/Rome",
  "Europe/Amsterdam",
  "Europe/Stockholm",
  "Europe/Zurich",
  "Europe/Warsaw",
  "Europe/Istanbul",
  "Europe/Kyiv",
  "Europe/Moscow",
  "Africa/Cairo",
  "Africa/Johannesburg",
  "America/Sao_Paulo",
  "America/Argentina/Buenos_Aires",
  "America/Mexico_City",
  "America/Bogota",
  "America/Santiago",
  "America/Los_Angeles",
  "America/Denver",
  "America/Chicago",
  "America/New_York",
  "America/Toronto",
  "America/Vancouver",
  "America/Anchorage",
  "Pacific/Honolulu",
  "UTC",
];

const partsFmtCache = new Map<string, Intl.DateTimeFormat>();

function partsFormatter(tz: string): Intl.DateTimeFormat {
  let fmt = partsFmtCache.get(tz);
  if (!fmt) {
    fmt = new Intl.DateTimeFormat("en-US", {
      timeZone: tz,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hourCycle: "h23",
    });
    partsFmtCache.set(tz, fmt);
  }
  return fmt;
}

/** UTC 인스턴트를 tz 의 로컬 파츠로 분해한다. */
export function zonedParts(epochMs: number, tz: string): ZonedParts {
  const parts = partsFormatter(tz).formatToParts(new Date(epochMs));
  const get = (type: Intl.DateTimeFormatPartTypes): number => {
    const p = parts.find((x) => x.type === type);
    return p ? Number.parseInt(p.value, 10) : 0;
  };
  return {
    y: get("year"),
    m: get("month"),
    d: get("day"),
    hh: get("hour"),
    mm: get("minute"),
    ss: get("second"),
  };
}

/** 인스턴트가 tz 에서 속한 로컬 날짜. */
export function localDateOf(epochMs: number, tz: string): LocalDate {
  const p = zonedParts(epochMs, tz);
  return { y: p.y, m: p.m, d: p.d };
}

/** 오늘 (tz 기준). */
export function todayInZone(tz: string): LocalDate {
  return localDateOf(Date.now(), tz);
}

/** ISO/RFC3339 문자열 → epoch ms. 파싱 불가면 NaN. */
export function parseInstant(iso: string): number {
  return Date.parse(iso);
}

/**
 * 로컬 날짜·시각을 UTC epoch ms 로 변환한다 — tz 의 실제 오프셋을 반복 추정해
 * DST 경계에서도 수렴한다. 존재하지 않는 현지 시각은 최대 3회 추정 후 마지막
 * 추정치(앞으로 밀린 인스턴트)를 반환한다.
 */
export function zonedTimeToUtc(
  date: LocalDate,
  hh: number,
  mm: number,
  tz: string
): number {
  const desired = Date.UTC(date.y, date.m - 1, date.d, hh, mm);
  let guess = desired;
  for (let i = 0; i < 4; i += 1) {
    const p = zonedParts(guess, tz);
    const actual = Date.UTC(p.y, p.m - 1, p.d, p.hh, p.mm);
    const diff = desired - actual;
    if (diff === 0) break;
    guess += diff;
  }
  return guess;
}

/** 로컬 날짜의 자정(0:00) UTC 인스턴트. */
export function localDayStartUtc(date: LocalDate, tz: string): number {
  return zonedTimeToUtc(date, 0, 0, tz);
}

/** 로컬 날짜 튜플을 days 만큼 이동 — 달력 연산이라 DST 안전하다. */
export function shiftLocalDate(date: LocalDate, days: number): LocalDate {
  const dt = new Date(Date.UTC(date.y, date.m - 1, date.d + days, 12));
  return { y: dt.getUTCFullYear(), m: dt.getUTCMonth() + 1, d: dt.getUTCDate() };
}

/** 로컬 날짜의 ISO 요일 (1=월 … 7=일 — availability_rules.weekday 와 동일). */
export function isoWeekdayOf(date: LocalDate): number {
  const jsDay = new Date(Date.UTC(date.y, date.m - 1, date.d)).getUTCDay();
  return jsDay === 0 ? 7 : jsDay;
}

/** date 가 속한 주의 월요일 (ISO week start). */
export function weekStartOf(date: LocalDate): LocalDate {
  return shiftLocalDate(date, -(isoWeekdayOf(date) - 1));
}

/** date 부터 days 개의 연속 로컬 날짜. */
export function localDateRange(start: LocalDate, days: number): LocalDate[] {
  return Array.from({ length: days }, (_, i) => shiftLocalDate(start, i));
}

export function sameLocalDate(a: LocalDate, b: LocalDate): boolean {
  return a.y === b.y && a.m === b.m && a.d === b.d;
}

export function localDateKey(date: LocalDate): string {
  const mm = String(date.m).padStart(2, "0");
  const dd = String(date.d).padStart(2, "0");
  return `${date.y}-${mm}-${dd}`;
}

/** 두 날짜 튜플 비교 — a<b:-1, 같음:0, a>b:1. */
export function compareLocalDate(a: LocalDate, b: LocalDate): number {
  return (
    Date.UTC(a.y, a.m - 1, a.d) - Date.UTC(b.y, b.m - 1, b.d)
  );
}

// ── 표시 포맷 ────────────────────────────────────────────────

const fmtCache = new Map<string, Intl.DateTimeFormat>();

function formatter(
  tz: string | undefined,
  locale: string,
  options: Intl.DateTimeFormatOptions
): Intl.DateTimeFormat {
  const key = `${locale}|${tz ?? ""}|${JSON.stringify(options)}`;
  let fmt = fmtCache.get(key);
  if (!fmt) {
    fmt = new Intl.DateTimeFormat(locale, { timeZone: tz, ...options });
    fmtCache.set(key, fmt);
  }
  return fmt;
}

export interface InstantFormatOptions {
  /** 생략하면 브라우저 시간대. */
  tz?: string;
  locale?: string;
}

/** "10/15 (수) 14:30" 식 날짜+시각. */
export function formatDateTime(
  iso: string | number | Date,
  { tz, locale = "ko" }: InstantFormatOptions = {}
): string {
  const at = typeof iso === "string" ? parseInstant(iso) : iso instanceof Date ? iso.getTime() : iso;
  if (Number.isNaN(at)) return String(iso);
  return formatter(tz, locale, {
    month: "numeric",
    day: "numeric",
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(at));
}

/** "14:30" */
export function formatTime(
  iso: string | number | Date,
  { tz, locale = "ko" }: InstantFormatOptions = {}
): string {
  const at = typeof iso === "string" ? parseInstant(iso) : iso instanceof Date ? iso.getTime() : iso;
  if (Number.isNaN(at)) return String(iso);
  return formatter(tz, locale, {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(at));
}

/** "2026-10-15" — 로케일 독립 ISO 형식으로 날짜만. */
export function formatIsoDate(
  iso: string | number | Date,
  { tz }: { tz?: string } = {}
): string {
  const at = typeof iso === "string" ? parseInstant(iso) : iso instanceof Date ? iso.getTime() : iso;
  if (Number.isNaN(at)) return String(iso);
  return localDateKey(localDateOf(at, tz ?? detectTimezone()));
}

/** "10월 15일 (수)" 식 요일+날짜 라벨. */
export function formatDayLabel(
  date: LocalDate,
  locale = "ko"
): string {
  const at = Date.UTC(date.y, date.m - 1, date.d, 12);
  return formatter("UTC", locale, {
    month: "short",
    day: "numeric",
    weekday: "short",
  }).format(new Date(at));
}

/** "GMT+9" / "KST" 식 시간대 이름 — DST 적용된 그 시각의 이름을 보인다. */
export function timezoneNameAt(
  iso: string | number | Date,
  tz: string,
  locale = "ko"
): string {
  const at = typeof iso === "string" ? parseInstant(iso) : iso instanceof Date ? iso.getTime() : iso;
  if (Number.isNaN(at)) return tz;
  const parts = new Intl.DateTimeFormat(locale, {
    timeZone: tz,
    timeZoneName: "short",
  }).formatToParts(new Date(at));
  return parts.find((p) => p.type === "timeZoneName")?.value ?? tz;
}

/** Explicit UTC offset for repeated local times during DST fall-back. */
export function timezoneOffsetAt(
  iso: string | number | Date,
  tz: string,
  locale = "ko"
): string {
  const at = typeof iso === "string" ? parseInstant(iso) : iso instanceof Date ? iso.getTime() : iso;
  if (Number.isNaN(at)) return tz;
  const parts = new Intl.DateTimeFormat(locale, {
    timeZone: tz,
    timeZoneName: "shortOffset",
  }).formatToParts(new Date(at));
  return parts.find((part) => part.type === "timeZoneName")?.value ?? tz;
}

/** datetime-local 입력값 "YYYY-MM-DDTHH:mm" → tz 해석한 UTC ISO 문자열. */
export function localInputToUtcIso(value: string, tz: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})/.exec(value);
  if (!m) return null;
  const date: LocalDate = {
    y: Number(m[1]),
    m: Number(m[2]),
    d: Number(m[3]),
  };
  const at = zonedTimeToUtc(date, Number(m[4]), Number(m[5]), tz);
  return new Date(at).toISOString();
}

/** UTC 인스턴트 → tz 로 표현한 datetime-local 입력값. */
export function utcToLocalInput(iso: string | number, tz: string): string {
  const at = typeof iso === "string" ? parseInstant(iso) : iso;
  if (Number.isNaN(at)) return "";
  const p = zonedParts(at, tz);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${p.y}-${pad(p.m)}-${pad(p.d)}T${pad(p.hh)}:${pad(p.mm)}`;
}
