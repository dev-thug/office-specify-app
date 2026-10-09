/**
 * KPI 카드 값 포맷 — §2.4 오해 방지 규칙.
 * - value === null 이면 "관측 불가"로 간주하고 절대 0·추정치로 대체하지 않는다.
 * - ratio 는 0..1 분수로 가정해 % 표기한다.
 * - seconds 는 가장 큰 단위로 반올림해 표기한다.
 */
import type { DashboardKpi, DashboardSummary } from "@/lib/api/dashboard";

export interface KpiTranslator {
  (key: string, values?: Record<string, string | number>): string;
}

/** null → null (호출부가 N/A 배지를 그린다). */
export function formatKpiValue(
  kpi: Pick<DashboardKpi, "unit" | "value">,
  locale: string,
  t: KpiTranslator
): string | null {
  if (kpi.value === null || kpi.value === undefined) return null;
  switch (kpi.unit) {
    case "ratio": {
      const pct = kpi.value * 100;
      // 정수 % 는 소수점 없이, 아니면 소수 첫째 자리까지 표기한다.
      const rounded = Number.isInteger(pct) ? pct.toString() : pct.toFixed(1);
      return `${rounded}%`;
    }
    case "seconds":
      return formatDurationSeconds(kpi.value, t);
    default:
      return new Intl.NumberFormat(locale).format(kpi.value);
  }
}

/** Convert the backend's structured KPI object into ordered cards without deriving missing data. */
export function dashboardKpiCards(summary: DashboardSummary): DashboardKpi[] {
  const window = { from: summary.period.from, to: summary.period.to };
  const refreshed_at = summary.generated_at;
  const { kpis } = summary;
  return [
    {
      key: "eligible_contacted_companies",
      unit: "count",
      value: kpis.eligible_contacted_companies,
      window,
      refreshed_at,
    },
    {
      key: "reply_company_ratio",
      unit: "ratio",
      numerator: kpis.reply_company_ratio.numerator,
      denominator: kpis.reply_company_ratio.denominator,
      value: kpis.reply_company_ratio.value,
      window,
      refreshed_at,
    },
    {
      key: "valid_meeting_ratio",
      unit: "ratio",
      numerator: kpis.meeting_conversion.numerator,
      denominator: kpis.meeting_conversion.denominator,
      value: kpis.meeting_conversion.value,
      window,
      refreshed_at,
    },
    {
      key: "first_reply_handling_seconds",
      unit: "seconds",
      value: kpis.first_reply.median_seconds,
      sample_size: kpis.first_reply.sample_size,
      window,
      refreshed_at,
    },
    {
      key: "agent_candidate_accept_ratio",
      unit: "ratio",
      numerator: kpis.agent_proposal_adoption.numerator,
      denominator: kpis.agent_proposal_adoption.denominator,
      value: kpis.agent_proposal_adoption.value,
      window,
      refreshed_at,
    },
  ];
}

export function formatDurationSeconds(seconds: number, t: KpiTranslator): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return t("secondsShort", { count: s });
  const m = Math.round(s / 60);
  if (m < 60) return t("minutesShort", { count: m });
  const h = Math.round(m / 60);
  if (h < 48) return t("hoursShort", { count: h });
  return t("daysShort", { count: Math.round(h / 24) });
}
