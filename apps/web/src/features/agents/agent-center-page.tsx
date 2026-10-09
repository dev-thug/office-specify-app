"use client";

import { useTranslations } from "next-intl";
import { useMemo, useState } from "react";

import { PageHeader } from "@/components/patterns/page-header";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { LeadProposalReviewList } from "@/features/research/lead-proposal-list";
import { useAgentPrincipals } from "@/lib/api/agents";

import { AgentRunsTable } from "./agent-runs-table";
import { AgentsTable } from "./agents-table";
import { NewRunDialog } from "./new-run-dialog";

/** UI10 Agent Center — 실행·리드 후보 검토·에이전트 탭. */
export function AgentCenterPage() {
  const t = useTranslations("agents.tabs");
  const tScreens = useTranslations("screens");
  const tRuns = useTranslations("agents.runs");
  const [newRunOpen, setNewRunOpen] = useState(false);

  // 실행 행에 표시할 에이전트 이름 맵 — 탭과 쿼리를 공유한다.
  const agentsQuery = useAgentPrincipals();
  const agentNames = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of agentsQuery.data?.pages.flatMap((pg) => pg.data) ?? []) {
      if (p.kind === "agent") map.set(p.id, p.display_name);
    }
    return map;
  }, [agentsQuery.data]);

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={tScreens("agents.title")}
        description={tScreens("agents.description")}
        actions={
          <Button size="sm" onClick={() => setNewRunOpen(true)}>
            {tRuns("newRun")}
          </Button>
        }
      />
      <Tabs defaultValue="runs">
        <TabsList>
          <TabsTrigger value="runs">{t("runs")}</TabsTrigger>
          <TabsTrigger value="proposals">{t("proposals")}</TabsTrigger>
          <TabsTrigger value="agents">{t("agents")}</TabsTrigger>
        </TabsList>
        <TabsContent value="runs" className="pt-4">
          <AgentRunsTable agentNames={agentNames} />
        </TabsContent>
        <TabsContent value="proposals" className="pt-4">
          <LeadProposalReviewList />
        </TabsContent>
        <TabsContent value="agents" className="pt-4">
          <AgentsTable />
        </TabsContent>
      </Tabs>
      <NewRunDialog open={newRunOpen} onOpenChange={setNewRunOpen} />
    </div>
  );
}
