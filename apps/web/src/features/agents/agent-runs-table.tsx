"use client";

import { useFormatter, useTranslations } from "next-intl";
import Link from "next/link";
import { useState } from "react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  useAgentRuns,
  type AgentRun,
  type AgentRunStatus,
} from "@/lib/api/agents";

const STATUS_OPTIONS: (AgentRunStatus | "all")[] = [
  "all",
  "queued",
  "running",
  "awaiting_approval",
  "completed",
  "failed",
  "canceled",
];

interface AgentRunsTableProps {
  /** principal_id → 표시 이름 (에이전트 목록 쿼리 결과로 만든다). */
  agentNames?: Map<string, string>;
}

/** 실행 목록 — 상태 필터 + 커서 페이지네이션 (UI10). */
export function AgentRunsTable({ agentNames }: AgentRunsTableProps) {
  const t = useTranslations("agents.runs");
  const tDetail = useTranslations("agents.detail");
  const tc = useTranslations("common");
  const tStatus = useTranslations("status");
  const format = useFormatter();
  const [status, setStatus] = useState<AgentRunStatus | "all">("all");
  const query = useAgentRuns({ status });

  const rows = query.data?.pages.flatMap((p) => p.data) ?? [];
  const last = query.data?.pages.at(-1);

  const columns: ColumnDef<AgentRun>[] = [
    {
      key: "objective",
      header: t("columns.objective"),
      cell: (run) => (
        <span className="line-clamp-2 max-w-md font-medium">{run.objective}</span>
      ),
    },
    {
      key: "agent",
      header: t("columns.agent"),
      cell: (run) => (
        <span className="text-muted-foreground">
          {agentNames?.get(run.principal_id) ?? tc("nameUnavailable")}
        </span>
      ),
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (run) => (
        <div className="flex flex-wrap items-center gap-1.5">
          <StatusBadge status={run.status} />
          {run.cancel_requested_at ? (
            <StatusBadge status="canceling" label={tDetail("cancelRequested")} />
          ) : null}
        </div>
      ),
    },
    {
      key: "createdAt",
      header: t("columns.createdAt"),
      cell: (run) => (
        <span className="text-muted-foreground">
          {format.dateTime(new Date(run.created_at), {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </span>
      ),
    },
    {
      key: "actions",
      header: t("columns.actions"),
      cell: (run) => (
        <Button variant="outline" size="xs" asChild>
          <Link href={`/app/agents/runs/${run.id}`}>{t("detail")}</Link>
        </Button>
      ),
    },
  ];

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Label htmlFor="run-status-filter" className="sr-only">
          {t("filterStatus")}
        </Label>
        <Select
          value={status}
          onValueChange={(v) => setStatus(v as AgentRunStatus | "all")}
        >
          <SelectTrigger
            id="run-status-filter"
            size="sm"
            aria-label={t("filterStatus")}
            className="w-44"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {STATUS_OPTIONS.map((s) => (
              <SelectItem key={s} value={s}>
                {s === "all" ? t("allStatuses") : tStatus(s)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(run) => run.id}
        page={last?.page}
        onLoadMore={() => query.fetchNextPage()}
        loadingMore={query.isFetchingNextPage}
        loading={query.isPending}
        error={query.error}
        onRetry={() => query.refetch()}
        emptyState={
          <EmptyState
            title={t("empty")}
            description={t("emptyDescription")}
            compact
          />
        }
      />
    </div>
  );
}
