/** RFC 3339 → 로케일 표시용 짧은 날짜·시각. 파싱 불가 값은 원문을 그대로 둔다. */
export function formatDateTime(
  value: string | null | undefined,
  locale: string
): string {
  if (!value) return "—";
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return value;
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(d);
}

/** sha256 hex 등 긴 식별자의 앞부분만 보여준다. */
export function shortHash(value: string | null | undefined, head = 12): string {
  if (!value) return "—";
  return value.length > head ? `${value.slice(0, head)}…` : value;
}
