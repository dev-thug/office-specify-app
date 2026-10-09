"use client";

import { useQuery } from "@tanstack/react-query";
import { useFormatter, useTranslations } from "next-intl";
import { useState, type ReactNode } from "react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { LeadProposalReviewList } from "@/features/research/lead-proposal-list";
import { ApiError } from "@/lib/api/problem";
import {
  getAgentRun,
  type AgentRunDetail as AgentRunDetailData,
} from "@/lib/api/agents";

import { AgentActionsTable } from "./agent-actions-table";
import { CancelRunDialog } from "./cancel-run-dialog";
import { canCancelRun, isActiveRunStatus } from "./run-state";

interface AgentRunDetailProps {
  runId: string;
  /** principal_id → 표시 이름. */
  agentNames?: Map<string, string>;
}

/** 실행 상세 — 진행 상태·도구 호출·budget/usage·중단 (UI10). */
export function AgentRunDetail({ runId, agentNames }: AgentRunDetailProps) {
  const t = useTranslations("agents.detail");
  const tScreens = useTranslations("screens");
  const format = useFormatter();
  const [cancelOpen, setCancelOpen] = useState(false);

  const query = useQuery({
    queryKey: ["agent-run", runId],
    queryFn: () => getAgentRun(runId),
    // 진행 중인 실행은 주기적으로 다시 읽는다 — 종료 상태면 중단한다.
    refetchInterval: (q) => {
      const run = q.state.data?.data;
      return run && isActiveRunStatus(run.status) ? 5_000 : false;
    },
  });

  if (query.isPending) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-40 w-full rounded-2xl" />
        <Skeleton className="h-64 w-full rounded-2xl" />
      </div>
    );
  }

  if (query.isError || !query.data) {
    const err = query.error;
    const notFound = err instanceof ApiError && err.status === 404;
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        {notFound ? (
          <EmptyState title={t("notFound")} />
        ) : (
          <ErrorState
            title={t("loadFailed")}
            error={err}
            onRetry={() => query.refetch()}
          />
        )}
      </div>
    );
  }

  const run = query.data.data;
  const actions = run.actions ?? [];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={tScreens("agentRunDetail.title")}
        description={tScreens("agentRunDetail.description")}
        actions={
          <div className="flex items-center gap-2">
            <StatusBadge status={run.status} />
            {canCancelRun(run) ? (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCancelOpen(true)}
              >
                {t("cancel")}
              </Button>
            ) : null}
          </div>
        }
      />

      <Card>
        <CardHeader>
          <CardTitle>{t("runInfo")}</CardTitle>
        </CardHeader>
        <CardContent>
          <dl className="grid grid-cols-1 gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
            <Field label={t("objective")}>
              <span className="font-medium">{run.objective}</span>
            </Field>
            <Field label={t("agent")}>
              {agentNames?.get(run.principal_id) ?? run.principal_id}
            </Field>
            <Field label={t("requestedBy")}>{run.requested_by}</Field>
            <Field label={t("createdAt")}>
              {format.dateTime(new Date(run.created_at), {
                dateStyle: "medium",
                timeStyle: "short",
              })}
            </Field>
            <Field label={t("startedAt")}>
              {run.started_at
                ? format.dateTime(new Date(run.started_at), {
                    dateStyle: "medium",
                    timeStyle: "short",
                  })
                : "—"}
            </Field>
            <Field label={t("finishedAt")}>
              {run.finished_at
                ? format.dateTime(new Date(run.finished_at), {
                    dateStyle: "medium",
                    timeStyle: "short",
                  })
                : "—"}
            </Field>
            {run.cancel_requested_at ? (
              <Field label={t("cancelRequestedAt")}>
                {format.dateTime(new Date(run.cancel_requested_at), {
                  dateStyle: "medium",
                  timeStyle: "short",
                })}
              </Field>
            ) : null}
          </dl>
        </CardContent>
      </Card>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        <BudgetCard title={t("budgetTitle")} values={run.budget} />
        <UsageCard run={run} />
      </div>

      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold">{t("actionsTitle")}</h2>
        <AgentActionsTable actions={actions} />
      </section>

      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold">{t("proposalsTitle")}</h2>
        <LeadProposalReviewList agentRunId={run.id} />
      </section>

      <CancelRunDialog run={run} open={cancelOpen} onOpenChange={setCancelOpen} />
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[8rem_1fr] items-baseline gap-2 sm:grid-cols-[10rem_1fr]">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

const KNOWN_BUDGET_KEYS = new Set([
  "max_tool_calls",
  "max_proposals",
  "max_external_searches",
  "max_input_tokens",
  "max_output_tokens",
  "max_cost_usd",
]);

const KNOWN_USAGE_KEYS = new Set([
  "tool_calls",
  "proposals_submitted",
  "external_searches",
  "input_tokens",
  "output_tokens",
  "estimated_cost_usd",
  "actual_cost_usd",
]);

/** 상한 카드 — 알려진 키는 번역 라벨, 그 외는 원시 키로 표시한다. */
function BudgetCard({
  title,
  values,
}: {
  title: string;
  values: Record<string, unknown>;
}) {
  const t = useTranslations("agents.detail.budgetKey");
  const entries = Object.entries(values ?? {});
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="text-sm text-muted-foreground">—</p>
        ) : (
          <dl className="grid gap-1.5 text-sm">
            {entries.map(([key, value]) => (
              <div key={key} className="flex items-baseline justify-between gap-3">
                <dt className="text-muted-foreground">
                  {KNOWN_BUDGET_KEYS.has(key) && t.has(key) ? t(key) : key}
                </dt>
                <dd className="font-medium">{formatMetric(key, value)}</dd>
              </div>
            ))}
          </dl>
        )}
      </CardContent>
    </Card>
  );
}

/**
 * 사용량 카드 — 비용은 추정(estimated)과 실측(actual)을 분리해 표시한다.
 * 실측이 없으면 "추정치" 배지를 붙인다 (§13.5, UI10).
 */
function UsageCard({ run }: { run: AgentRunDetailData }) {
  const t = useTranslations("agents.detail");
  const usage = run.usage ?? {};
  const entries = Object.entries(usage).filter(
    ([key]) => key !== "actual_cost_usd" && key !== "estimated_cost_usd"
  );
  const estimated = usage.estimated_cost_usd;
  const actual = usage.actual_cost_usd;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("usageTitle")}</CardTitle>
      </CardHeader>
      <CardContent>
        <dl className="grid gap-1.5 text-sm">
          {entries.map(([key, value]) => (
            <div key={key} className="flex items-baseline justify-between gap-3">
              <dt className="text-muted-foreground">
                <UsageKeyLabel k={key} />
              </dt>
              <dd className="font-medium">{formatMetric(key, value)}</dd>
            </div>
          ))}
          {estimated !== undefined ? (
            <div className="flex items-baseline justify-between gap-3">
              <dt className="text-muted-foreground">
                <UsageKeyLabel k="estimated_cost_usd" />
              </dt>
              <dd className="flex items-center gap-1.5 font-medium">
                {formatMetric("estimated_cost_usd", estimated)}
                {actual === undefined ? (
                  <Badge variant="amber-subtle">{t("costEstimatedBadge")}</Badge>
                ) : null}
              </dd>
            </div>
          ) : null}
          {actual !== undefined ? (
            <div className="flex items-baseline justify-between gap-3">
              <dt className="text-muted-foreground">
                <UsageKeyLabel k="actual_cost_usd" />
              </dt>
              <dd className="font-medium">
                {formatMetric("actual_cost_usd", actual)}
              </dd>
            </div>
          ) : null}
        </dl>
        {estimated !== undefined && actual === undefined ? (
          <p className="mt-2 text-2xs text-muted-foreground">
            {t("costEstimateOnly")}
          </p>
        ) : null}
        {entries.length === 0 &&
        estimated === undefined &&
        actual === undefined ? (
          <p className="text-sm text-muted-foreground">—</p>
        ) : null}
      </CardContent>
    </Card>
  );
}

function UsageKeyLabel({ k }: { k: string }) {
  const t = useTranslations("agents.detail.usageKey");
  return KNOWN_USAGE_KEYS.has(k) && t.has(k) ? t(k) : k;
}

function formatMetric(key: string, value: unknown): string {
  if (typeof value === "number") {
    return key.endsWith("_usd") || key.includes("cost")
      ? `$${value.toLocaleString(undefined, { maximumFractionDigits: 4 })}`
      : value.toLocaleString();
  }
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}
