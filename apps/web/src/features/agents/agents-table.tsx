"use client";

import { useTranslations } from "next-intl";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import {
  useAgentPrincipals,
  type AgentPrincipal,
} from "@/lib/api/agents";

/** 에이전트 principal 목록 — principals 응답에서 kind=agent 만 표시한다 (UI10). */
export function AgentsTable() {
  const t = useTranslations("agents.agentsList");
  const query = useAgentPrincipals();

  const rows = (query.data?.pages.flatMap((p) => p.data) ?? []).filter(
    (p) => p.kind === "agent"
  );
  const last = query.data?.pages.at(-1);

  const columns: ColumnDef<AgentPrincipal>[] = [
    {
      key: "name",
      header: t("columns.name"),
      cell: (p) => <span className="font-medium">{p.display_name}</span>,
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (p) => <StatusBadge status={p.status} />,
    },
    {
      key: "scopes",
      header: t("columns.scopes"),
      cell: (p) =>
        p.scopes && p.scopes.length > 0 ? (
          <span className="flex flex-wrap gap-1">
            {p.scopes.map((scope) => (
              <Badge key={scope} variant="secondary">
                {scope}
              </Badge>
            ))}
          </span>
        ) : (
          <span className="text-muted-foreground">{t("noScopes")}</span>
        ),
    },
    {
      key: "id",
      header: t("columns.id"),
      cell: (p) => (
        <span className="font-mono text-2xs text-muted-foreground">{p.id}</span>
      ),
    },
  ];

  return (
    <DataTable
      columns={columns}
      rows={rows}
      rowKey={(p) => p.id}
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
  );
}
