"use client";

import { useMemo } from "react";

import { useAgentPrincipals } from "@/lib/api/agents";

import { AgentRunDetail } from "./agent-run-detail";

/** 실행 상세 라우트 엔트리 — 에이전트 이름 맵을 만들어 상세에 넘긴다. */
export function AgentRunDetailPage({ runId }: { runId: string }) {
  const agentsQuery = useAgentPrincipals();
  const agentNames = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of agentsQuery.data?.pages.flatMap((pg) => pg.data) ?? []) {
      if (p.kind === "agent") map.set(p.id, p.display_name);
    }
    return map;
  }, [agentsQuery.data]);

  return <AgentRunDetail runId={runId} agentNames={agentNames} />;
}
