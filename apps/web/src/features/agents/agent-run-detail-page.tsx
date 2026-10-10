"use client";

import { useMemo } from "react";

import { useAgentPrincipals } from "@/lib/api/agents";
import { usePrincipals } from "@/lib/api/settings";

import { AgentRunDetail } from "./agent-run-detail";

/** 실행 상세 라우트 엔트리 — 에이전트 이름 맵을 만들어 상세에 넘긴다. */
export function AgentRunDetailPage({ runId }: { runId: string }) {
  const agentsQuery = useAgentPrincipals();
  const principalsQuery = usePrincipals();
  const agentNames = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of agentsQuery.data?.pages.flatMap((pg) => pg.data) ?? []) {
      const name = p.display_name?.trim() || p.name?.trim();
      if (p.kind === "agent" && name) map.set(p.id, name);
    }
    return map;
  }, [agentsQuery.data]);

  const principalNames = useMemo(() => {
    const map = new Map<string, string>();
    for (const principal of principalsQuery.data?.pages.flatMap((pg) => pg.data) ?? []) {
      const name = principal.name?.trim();
      if (name) map.set(principal.id, name);
    }
    return map;
  }, [principalsQuery.data]);

  return <AgentRunDetail runId={runId} agentNames={agentNames} principalNames={principalNames} />;
}
