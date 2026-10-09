import { describe, expect, it } from "vitest";

import { dashboardKpiCards, formatDurationSeconds, formatKpiValue, type KpiTranslator } from "@/features/dashboard/kpi";

const t: KpiTranslator = (key, values) =>
  `${key}${values ? `:${JSON.stringify(values)}` : ""}`;

describe("formatKpiValue — 모르는 결과는 N/A", () => {
  it("returns null for null/undefined values so the card renders N/A, never 0", () => {
    expect(formatKpiValue({ unit: "ratio", value: null }, "ko", t)).toBeNull();
    expect(formatKpiValue({ unit: "count", value: null }, "ko", t)).toBeNull();
  });

  it("formats ratio as a percentage", () => {
    expect(formatKpiValue({ unit: "ratio", value: 0.123 }, "ko", t)).toBe("12.3%");
    expect(formatKpiValue({ unit: "ratio", value: 0.42 }, "ko", t)).toBe("42%");
  });

  it("formats counts with the locale number format", () => {
    expect(formatKpiValue({ unit: "count", value: 1234 }, "en", t)).toBe("1,234");
  });

  it("formats seconds via the duration ladder", () => {
    expect(formatKpiValue({ unit: "seconds", value: 90 }, "ko", t)).toContain("minutesShort");
  });
});

describe("formatDurationSeconds", () => {
  it("climbs s→m→h→d", () => {
    expect(formatDurationSeconds(30, t)).toContain("secondsShort");
    expect(formatDurationSeconds(300, t)).toContain("minutesShort");
    expect(formatDurationSeconds(7200, t)).toContain("hoursShort");
    expect(formatDurationSeconds(200000, t)).toContain("daysShort");
  });
});

describe("dashboardKpiCards — backend KPI object adapter", () => {
  it("maps the real response DTO without inventing metrics or losing denominators and freshness", () => {
    const generatedAt = "2026-10-09T10:15:00Z";
    const from = "2026-09-09T10:15:00Z";
    const to = generatedAt;
    const cards = dashboardKpiCards({
      period: { from, to },
      generated_at: generatedAt,
      scope: "mine",
      kpis: {
        eligible_contacted_companies: 23,
        reply_company_ratio: { numerator: 4, denominator: 23, value: 4 / 23 },
        meeting_conversion: { numerator: 0, denominator: 23, value: 0 },
        first_reply: { sample_size: 0, median_seconds: null },
        agent_proposal_adoption: { numerator: 1, denominator: 2, value: 0.5 },
      },
      operational: {
        unknown_outbound_messages: 1,
        unknown_jobs: 2,
        dead_jobs: 3,
        outbox_backlog: 4,
        reauth_required_mailboxes: 5,
        emergency_stop: false,
        send_workers_locked: false,
      },
      queues: [],
    });

    expect(cards).toEqual([
      {
        key: "eligible_contacted_companies",
        unit: "count",
        value: 23,
        window: { from, to },
        refreshed_at: generatedAt,
      },
      {
        key: "reply_company_ratio",
        unit: "ratio",
        numerator: 4,
        denominator: 23,
        value: 4 / 23,
        window: { from, to },
        refreshed_at: generatedAt,
      },
      {
        key: "valid_meeting_ratio",
        unit: "ratio",
        numerator: 0,
        denominator: 23,
        value: 0,
        window: { from, to },
        refreshed_at: generatedAt,
      },
      {
        key: "first_reply_handling_seconds",
        unit: "seconds",
        value: null,
        sample_size: 0,
        window: { from, to },
        refreshed_at: generatedAt,
      },
      {
        key: "agent_candidate_accept_ratio",
        unit: "ratio",
        numerator: 1,
        denominator: 2,
        value: 0.5,
        window: { from, to },
        refreshed_at: generatedAt,
      },
    ]);
    expect(cards.some((kpi) => kpi.key === "delivery_unknown_ratio")).toBe(false);
  });
});
