"use client";

/**
 * UI01 대시보드 — 작업 큐 + 코호트 KPI.
 * 규칙: 카드마다 분모·관측 기간·갱신 시각 표시, 모르는 전달률은 N/A,
 * "수락됨"을 "전달됨/읽음"으로 표시하지 않는다 (§2.4).
 */
import {
  CalendarDaysIcon,
  ClockIcon,
  InfoIcon,
  InboxIcon,
  ListChecksIcon,
  OctagonXIcon,
  RefreshCwIcon,
  TriangleAlertIcon,
  type LucideIcon,
} from "lucide-react";
import Link from "next/link";
import { useLocale, useTranslations } from "next-intl";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  getDashboardSummary,
  type DashboardKpi,
  type DashboardQueue,
  type DashboardScope,
} from "@/lib/api/dashboard";
import { getMe } from "@/lib/api/auth";
import { getEmergencyStop } from "@/lib/api/operations";
import { dashboardKpiCards, formatKpiValue } from "@/features/dashboard/kpi";
import { formatDate, formatDateTime } from "@/features/settings/format";

/** 큐 key → 이동 영역·아이콘. 모르는 key 는 라벨만 보이는 정적 카드로 떨어진다. */
const QUEUE_META: Record<string, { href: string; icon: LucideIcon }> = {
  inbox_unhandled: { href: "/app/inbox", icon: InboxIcon },
  approvals_pending: { href: "/app/approvals", icon: ListChecksIcon },
  meetings_today: { href: "/app/meetings", icon: CalendarDaysIcon },
  outbound_unknown: { href: "/app/operations/jobs", icon: TriangleAlertIcon },
  sync_lag: { href: "/app/operations/integrations", icon: RefreshCwIcon },
  reauth_required: { href: "/app/settings/mail-accounts", icon: TriangleAlertIcon },
  dead_jobs: { href: "/app/operations/jobs", icon: TriangleAlertIcon },
  outbox_backlog: { href: "/app/operations/integrations", icon: TriangleAlertIcon },
};

function KpiCard({ kpi }: { kpi: DashboardKpi }) {
  const t = useTranslations("dashboard");
  const locale = useLocale();
  const label = t.has(`kpi.${kpi.key}`) ? t(`kpi.${kpi.key}`) : (kpi.label ?? kpi.key);
  const value = formatKpiValue(kpi, locale, t);
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="pb-1">
        <CardTitle className="text-xs font-medium text-muted-foreground">
          {label}
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-1.5">
        {value === null ? (
          <div className="flex items-center gap-2">
            <span className="text-xl font-semibold">{t("notAvailable")}</span>
            <Badge variant="amber-subtle" title={t("notAvailableReason")}>
              <InfoIcon aria-hidden />
              {t("notAvailable")}
            </Badge>
          </div>
        ) : (
          <span className="text-xl font-semibold tabular-nums">{value}</span>
        )}
        <dl className="flex flex-col gap-0.5 text-2xs text-muted-foreground">
          {kpi.denominator !== undefined && kpi.denominator !== null ? (
            <div>
              <dt className="sr-only">denominator</dt>
              <dd>{t("denominator", { count: kpi.denominator })}</dd>
            </div>
          ) : null}
          {kpi.sample_size !== undefined && kpi.sample_size !== null ? (
            <div>
              <dt className="sr-only">sample size</dt>
              <dd>{t("sampleSize", { count: kpi.sample_size })}</dd>
            </div>
          ) : null}
          {kpi.window ? (
            <div>
              <dt className="sr-only">window</dt>
              <dd>
                {t("observationWindow", {
                  from: formatDate(kpi.window.from, locale),
                  to: formatDate(kpi.window.to, locale),
                })}
              </dd>
            </div>
          ) : null}
          {kpi.refreshed_at ? (
            <div>
              <dt className="sr-only">refreshed</dt>
              <dd>{t("refreshedAt", { time: formatDateTime(kpi.refreshed_at, locale) })}</dd>
            </div>
          ) : null}
        </dl>
      </CardContent>
    </Card>
  );
}

function QueueCard({ queue }: { queue: DashboardQueue }) {
  const t = useTranslations("dashboard");
  const locale = useLocale();
  const meta = QUEUE_META[queue.key];
  const label = t.has(`queue.${queue.key}`) ? t(`queue.${queue.key}`) : t("queue.other");
  const Icon = meta?.icon ?? ClockIcon;
  const inner = (
    <Card className="h-full py-3 transition-colors hover:bg-accent/40">
      <CardContent className="flex items-center gap-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground">
          <Icon className="size-4" aria-hidden />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">{label}</span>
          {queue.oldest_at ? (
            <span className="block text-2xs text-muted-foreground">
              {formatDateTime(queue.oldest_at, locale)}
            </span>
          ) : null}
        </span>
        <span className="shrink-0 text-2xl font-semibold tabular-nums">{queue.count}</span>
      </CardContent>
    </Card>
  );
  if (!meta) return <div className="h-full">{inner}</div>;
  return (
    <Link
      href={meta.href}
      aria-label={t("queueItemAria", { label, count: queue.count })}
      className="block h-full rounded-2xl outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px]"
    >
      {inner}
    </Link>
  );
}

function OperationalCard({
  labelKey,
  count,
  href,
}: {
  labelKey: string;
  count: number;
  href: string;
}) {
  const t = useTranslations("dashboard");
  const label = t(`operational.${labelKey}`);
  return (
    <Link
      href={href}
      aria-label={t("queueItemAria", { label, count })}
      className="block rounded-2xl outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px]"
    >
      <Card className="h-full py-3 transition-colors hover:bg-accent/40">
        <CardContent className="flex items-center gap-3">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground">
            <TriangleAlertIcon className="size-4" aria-hidden />
          </span>
          <span className="min-w-0 flex-1 text-sm font-medium">{label}</span>
          <span className="shrink-0 text-2xl font-semibold tabular-nums">{count}</span>
        </CardContent>
      </Card>
    </Link>
  );
}

export function DashboardScreen() {
  const t = useTranslations("dashboard");
  const locale = useLocale();
  const [scope, setScope] = useState<DashboardScope>("mine");
  const me = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });
  const canViewTeam = me.data?.role === "admin" || me.data?.role === "manager";

  const summary = useQuery({
    queryKey: ["dashboard", "summary", scope],
    queryFn: () => getDashboardSummary({ scope }),
  });
  // 긴급 중지는 최상단 차단 배너 — 상태 조회 실패는 배너만 숨기고 화면을 막지 않는다.
  const stop = useQuery({
    queryKey: ["operations", "emergency-stop"],
    queryFn: getEmergencyStop,
    retry: false,
  });
  const stopped = stop.data?.data.enabled === true;

  const data = summary.data?.data;
  const queues = data?.queues ?? [];
  const kpis = data ? dashboardKpiCards(data) : [];
  const queueTotal = queues.reduce((acc, q) => acc + (q.count || 0), 0);
  const operational = data?.operational;
  const visibleQueueKeys = new Set(queues.map((queue) => queue.key));
  const operationalCards = operational
    ? [
        {
          queueKey: "outbound_unknown",
          labelKey: "unknownOutboundMessages",
          count: operational.unknown_outbound_messages,
          href: "/app/operations/jobs",
        },
        {
          queueKey: "unknown_jobs",
          labelKey: "unknownJobs",
          count: operational.unknown_jobs,
          href: "/app/operations/jobs",
        },
        {
          queueKey: "dead_jobs",
          labelKey: "deadJobs",
          count: operational.dead_jobs,
          href: "/app/operations/jobs",
        },
        {
          queueKey: "outbox_backlog",
          labelKey: "outboxBacklog",
          count: operational.outbox_backlog,
          href: "/app/operations/integrations",
        },
        {
          queueKey: "reauth_required",
          labelKey: "reauthRequiredMailboxes",
          count: operational.reauth_required_mailboxes,
          href: "/app/settings/mail-accounts",
        },
      ].filter((item) => !visibleQueueKeys.has(item.queueKey))
    : [];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <Tabs
            value={scope}
            onValueChange={(v) =>
              setScope(v === "team" && canViewTeam ? "team" : "mine")
            }
            aria-label={t("scopeLabel")}
          >
            <TabsList>
              <TabsTrigger value="mine">{t("scope.mine")}</TabsTrigger>
              {canViewTeam ? (
                <TabsTrigger value="team">{t("scope.team")}</TabsTrigger>
              ) : null}
            </TabsList>
          </Tabs>
        }
      />

      {stopped || operational?.emergency_stop ? (
        <Alert variant="warning" role="alert">
          <OctagonXIcon />
          <AlertTitle>{t("emergencyStopBanner")}</AlertTitle>
          <AlertDescription>
            <Link href="/app/operations/jobs" className="font-medium underline underline-offset-2">
              {t("emergencyStopLink")}
            </Link>
          </AlertDescription>
        </Alert>
      ) : null}
      {operational?.send_workers_locked ? (
        <Alert variant="warning" role="alert">
          <OctagonXIcon />
          <AlertTitle>{t("sendWorkersLockedBanner")}</AlertTitle>
          <AlertDescription>
            <Link href="/app/operations/jobs" className="font-medium underline underline-offset-2">
              {t("emergencyStopLink")}
            </Link>
          </AlertDescription>
        </Alert>
      ) : null}

      {summary.isPending ? (
        <div className="flex flex-col gap-5" aria-busy="true" aria-label={t("title")}>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 5 }).map((_, i) => (
              <Skeleton key={i} className="h-20 rounded-2xl" />
            ))}
          </div>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 xl:grid-cols-6">
            {Array.from({ length: 6 }).map((_, i) => (
              <Skeleton key={i} className="h-28 rounded-2xl" />
            ))}
          </div>
        </div>
      ) : summary.isError ? (
        <ErrorState error={summary.error} onRetry={() => summary.refetch()} />
      ) : (
        <>
          <section aria-labelledby="dashboard-queues">
            <h2 id="dashboard-queues" className="mb-2 text-sm font-medium text-muted-foreground">
              {t("queueSection")}
            </h2>
            {queueTotal === 0 && queues.length === 0 ? (
              <EmptyState
                title={t("queueEmptyTitle")}
                description={t("queueEmptyDescription")}
              />
            ) : (
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
                {queues.map((q) => (
                  <QueueCard key={q.key} queue={q} />
                ))}
              </div>
            )}
          </section>

          {operational ? (
            <section aria-labelledby="dashboard-operational">
              <h2 id="dashboard-operational" className="mb-2 text-sm font-medium text-muted-foreground">
                {t("operationalSection")}
              </h2>
              {operationalCards.length > 0 ? (
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
                  {operationalCards.map((item) => (
                    <OperationalCard
                      key={item.labelKey}
                      labelKey={item.labelKey}
                      count={item.count}
                      href={item.href}
                    />
                  ))}
                </div>
              ) : (
                <p className="text-sm text-muted-foreground">
                  {t("operationalAllInQueues")}
                </p>
              )}
            </section>
          ) : null}

          <section aria-labelledby="dashboard-kpis">
            <h2 id="dashboard-kpis" className="mb-2 text-sm font-medium text-muted-foreground">
              {t("kpiSection")}
            </h2>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 xl:grid-cols-6">
              {kpis.map((kpi) => (
                <KpiCard key={kpi.key} kpi={kpi} />
              ))}
            </div>
            <p className="mt-2 flex items-center gap-1.5 text-2xs text-muted-foreground">
              <InfoIcon className="size-3" aria-hidden />
              {t("unknownNotDelivered")}
              {data?.generated_at ? ` · ${t("refreshedAt", { time: formatDateTime(data.generated_at, locale) })}` : ""}
            </p>
          </section>
        </>
      )}
    </div>
  );
}
