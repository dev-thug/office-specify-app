"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useMemo, useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { createAgentRun, useAgentPrincipals } from "@/lib/api/agents";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

interface NewRunDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

function parsePositiveInt(value: string): number | undefined {
  const n = Number(value);
  return Number.isInteger(n) && n > 0 ? n : undefined;
}

/** run 요청 대화상자 — 목적·대상 범위·실행 상한 (§3.2, UI10). */
export function NewRunDialog({ open, onOpenChange }: NewRunDialogProps) {
  const t = useTranslations("agents.newRun");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const agentsQuery = useAgentPrincipals();
  const agents = useMemo(
    () => (agentsQuery.data?.pages.flatMap((page) => page.data ?? []) ?? []).filter((agent) => agent.kind === "agent" && agent.status === "active"),
    [agentsQuery.data]
  );
  const [selectedAgentId, setSelectedAgentId] = useState("");
  const agentId = agents.some((agent) => agent.id === selectedAgentId)
    ? selectedAgentId
    : agents[0]?.id ?? "";
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 논리 작업당 하나의 멱등 키 — 확정 응답 후에만 재발급한다 (DESIGN.md API usage).
  const idemRef = useRef<string>(idempotencyKey());

  async function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    const objective = String(form.get("objective") ?? "").trim();
    if (!objective) {
      setError(t("objectiveRequired"));
      return;
    }
    const industry = String(form.get("industry") ?? "").trim();
    const country = String(form.get("country") ?? "").trim();
    const maxProposals = parsePositiveInt(String(form.get("max_proposals") ?? ""));
    const maxToolCalls = parsePositiveInt(String(form.get("max_tool_calls") ?? ""));
    const maxSearches = parsePositiveInt(
      String(form.get("max_external_searches") ?? "")
    );
    const maxCost = Number(String(form.get("max_cost_usd") ?? ""));
    if (!agentId) {
      setError(t("agentRequired"));
      return;
    }

    setPending(true);
    setError(null);
    try {
      await createAgentRun(
        {
          agent_principal_id: agentId,
          objective,
          scope: {
            ...(industry ? { industry } : {}),
            ...(country ? { country_code: country } : {}),
          },
          budget: {
            ...(maxProposals ? { max_proposals: maxProposals } : {}),
            ...(maxToolCalls ? { max_tool_calls: maxToolCalls } : {}),
            ...(maxSearches ? { max_external_searches: maxSearches } : {}),
            ...(Number.isFinite(maxCost) && maxCost > 0
              ? { max_cost_usd: maxCost }
              : {}),
          },
        },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      toast.success(t("success"));
      await queryClient.invalidateQueries({ queryKey: ["agent-runs"] });
      onOpenChange(false);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      setError(apiErr?.problem.detail ?? t("error"));
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="grid gap-4">
          <div className="grid gap-1.5">
            <Label htmlFor="run-agent">{t("agent")}</Label>
            <Select value={agentId} onValueChange={setSelectedAgentId} disabled={agentsQuery.isPending || agents.length === 0}>
              <SelectTrigger id="run-agent"><SelectValue placeholder={t("selectAgent")} /></SelectTrigger>
              <SelectContent>{agents.map((agent) => <SelectItem key={agent.id} value={agent.id}>{agent.display_name}</SelectItem>)}</SelectContent>
            </Select>
            {agentsQuery.isError ? <p role="alert" className="text-xs text-destructive">{t("error")}</p> : null}
            {!agentsQuery.isPending && agents.length === 0 ? <p className="text-xs text-muted-foreground">{t("noAgents")}</p> : null}
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="run-objective">{t("objective")}</Label>
            <Textarea
              id="run-objective"
              name="objective"
              required
              rows={3}
              placeholder={t("objectivePlaceholder")}
            />
          </div>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <div className="grid gap-1.5">
              <Label htmlFor="run-industry">{t("scopeIndustry")}</Label>
              <Input id="run-industry" name="industry" />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="run-country">{t("scopeCountry")}</Label>
              <Input
                id="run-country"
                name="country"
                maxLength={2}
                className="uppercase"
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="run-max-proposals">{t("maxProposals")}</Label>
              <Input
                id="run-max-proposals"
                name="max_proposals"
                type="number"
                min={1}
                max={100}
                inputMode="numeric"
              />
            </div>
          </div>
          <fieldset className="grid gap-3 rounded-2xl border border-border px-4 py-3">
            <legend className="px-1 text-xs font-medium text-muted-foreground">
              {t("budgetTitle")}
            </legend>
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
              <div className="grid gap-1.5">
                <Label htmlFor="run-max-tool-calls">{t("budgetToolCalls")}</Label>
                <Input
                  id="run-max-tool-calls"
                  name="max_tool_calls"
                  type="number"
                  min={1}
                  inputMode="numeric"
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="run-max-searches">{t("budgetSearches")}</Label>
                <Input
                  id="run-max-searches"
                  name="max_external_searches"
                  type="number"
                  min={1}
                  inputMode="numeric"
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="run-max-cost">{t("budgetCost")}</Label>
                <Input
                  id="run-max-cost"
                  name="max_cost_usd"
                  type="number"
                  min={0}
                  step="0.01"
                  inputMode="decimal"
                />
              </div>
            </div>
          </fieldset>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              {tCommon("cancel")}
            </Button>
            <Button type="submit" disabled={pending || agentsQuery.isPending || agents.length === 0 || !agentId}>
              {pending ? t("submitting") : t("submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
