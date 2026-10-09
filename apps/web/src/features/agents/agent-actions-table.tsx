"use client";

import { useFormatter, useTranslations } from "next-intl";
import Link from "next/link";
import { TriangleAlert } from "lucide-react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type { AgentAction } from "@/lib/api/agents";

/**
 * 도구 호출 목록 — 도구명·결과 코드·외부 효과·승인 링크 (UI10).
 * 안전 규칙: 결과가 불명(unknown)인 외부 효과는 "조사 필요"로 표시하고
 * 재전송 버튼은 절대 제공하지 않는다 (§7.3 규칙).
 */
export function AgentActionsTable({ actions }: { actions: AgentAction[] }) {
  const t = useTranslations("agents.detail");
  const format = useFormatter();

  const columns: ColumnDef<AgentAction>[] = [
    {
      key: "tool",
      header: t("colTool"),
      cell: (a) => <span className="font-medium">{a.tool_name}</span>,
    },
    {
      key: "result",
      header: t("colResult"),
      cell: (a) => (
        <span className="font-mono text-ui text-muted-foreground">
          {a.result_code ?? "—"}
        </span>
      ),
    },
    {
      key: "external",
      header: t("colExternalEffect"),
      cell: (a) =>
        a.has_external_effect ? (
          a.result_code === "unknown" ? (
            <Badge variant="amber-subtle">
              <TriangleAlert aria-hidden /> {t("investigate")}
            </Badge>
          ) : (
            <Badge variant="info-subtle">{t("externalEffect")}</Badge>
          )
        ) : (
          <span className="text-muted-foreground text-ui">
            {t("internalOnly")}
          </span>
        ),
    },
    {
      key: "approval",
      header: t("colApproval"),
      cell: (a) =>
        a.approval_request_id ? (
          <Button variant="link" size="xs" asChild className="px-0">
            <Link href={`/app/approvals/${a.approval_request_id}`}>
              {t("viewApproval")}
            </Link>
          </Button>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      key: "started",
      header: t("colStarted"),
      cell: (a) => (
        <span className="text-muted-foreground">
          {format.dateTime(new Date(a.started_at), {
            dateStyle: "medium",
            timeStyle: "medium",
          })}
        </span>
      ),
    },
    {
      key: "duration",
      header: t("colDuration"),
      cell: (a) => (
        <span className="text-muted-foreground">
          {formatDuration(a.started_at, a.finished_at)}
        </span>
      ),
    },
  ];

  return (
    <DataTable
      columns={columns}
      rows={actions}
      rowKey={(a) => a.id}
      emptyState={<EmptyState title={t("actionsEmpty")} compact />}
    />
  );
}

function formatDuration(started: string, finished: string | null): string {
  if (!finished) return "—";
  const ms = new Date(finished).getTime() - new Date(started).getTime();
  if (!Number.isFinite(ms) || ms < 0) return "—";
  if (ms < 1000) return `${ms}ms`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m ${s % 60}s`;
}
