"use client";

/**
 * 에이전트 + 자격증명 관리 (§5.2.5-5.2.8).
 * credential_secret 은 생성 응답에서 정확히 한 번만 표시하고 저장하지 않는다.
 */
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  createAgent,
  createAgentCredential,
  credentialSecret,
  revokeCredential,
  useAgentCredentials,
  usePrincipals,
  type Principal,
  type ServiceCredential,
  type ServiceCredentialCreated,
} from "@/lib/api/settings";
import { apiErrorText } from "@/features/settings/api-error";
import { CopyField } from "@/features/settings/copy-field";
import { formatDate, formatDateTime } from "@/features/settings/format";
import { useIsAdmin } from "@/features/settings/use-me";

export const AGENT_SCOPES = [
  "crm.read",
  "crm.write",
  "crm.propose",
  "research.propose",
  "research.write",
  "templates.read",
  "templates.write",
  "mail.read",
  "mail.compose",
  "campaign.compose",
  "campaign.pause",
  "booking.read",
  "booking.propose",
  "approvals.read",
  "operations.read",
  "agents.run",
] as const;

export function AgentsSection() {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);

  const agents = usePrincipals({ kind: "agent" });
  const rows = agents.data?.pages.flatMap((p) => p.data) ?? [];
  const page = agents.data?.pages.at(-1)?.page;

  return (
    <section className="flex flex-col gap-3" aria-labelledby="agents-heading">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="agents-heading" className="text-sm font-medium">
          {t("sectionAgents")}
        </h2>
        {isAdmin ? (
          <Button type="button" variant="outline" size="sm" onClick={() => setCreateOpen(true)}>
            {t("agents.new")}
          </Button>
        ) : null}
      </div>
      {agents.isPending ? (
        <div className="flex flex-col gap-3" aria-hidden="true">
          <div className="h-28 animate-pulse rounded-2xl bg-muted motion-reduce:animate-none" />
          <div className="h-28 animate-pulse rounded-2xl bg-muted motion-reduce:animate-none" />
        </div>
      ) : agents.isError ? (
        <ErrorInline
          message={apiErrorText(tSettings, agents.error)}
          onRetry={() => agents.refetch()}
        />
      ) : rows.length === 0 ? (
        <EmptyState title={t("agents.emptyTitle")} description={t("agents.emptyDescription")} />
      ) : (
        <ul className="flex flex-col gap-3">
          {rows.map((agent) => (
            <li
              key={agent.id}
              className="flex flex-col gap-3 rounded-2xl border border-border bg-card p-4"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  <span className="font-medium">{agent.name}</span>
                  <StatusBadge status={agent.status} />
                  <span className="text-2xs text-muted-foreground">
                    {formatDate(agent.created_at, locale)}
                  </span>
                </div>
                {isAdmin && agent.status === "active" ? (
                  <NewCredentialButton agent={agent} />
                ) : null}
              </div>
              <AgentCredentials agent={agent} />
            </li>
          ))}
        </ul>
      )}
      {page?.has_more ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="self-center"
          disabled={agents.isFetchingNextPage}
          onClick={() => agents.fetchNextPage()}
        >
          {t("loadMore")}
        </Button>
      ) : null}
      <NewAgentDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={() => queryClient.invalidateQueries({ queryKey: ["settings", "principals"] })}
      />
    </section>
  );
}

function ErrorInline({ message, onRetry }: { message: string; onRetry: () => void }) {
  const tCommon = useTranslations("common");
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-destructive/30 bg-destructive-subtle px-3 py-2"
    >
      <span className="text-xs text-destructive">{message}</span>
      <Button type="button" variant="outline" size="xs" onClick={onRetry}>
        {tCommon("retry")}
      </Button>
    </div>
  );
}

function AgentCredentials({ agent }: { agent: Principal }) {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const [revokeFor, setRevokeFor] = useState<ServiceCredential | null>(null);

  const credentials = useAgentCredentials(agent.id);
  const rows = credentials.data?.pages.flatMap((p) => p.data) ?? [];

  const revoke = useMutation({
    mutationFn: (id: string) =>
      revokeCredential(id, { idempotencyKey: idempotencyKey() }),
    onSuccess: () => {
      toast.success(t("credentials.revokeSuccess"));
      queryClient.invalidateQueries({
        queryKey: ["settings", "agents", agent.id, "credentials"],
      });
    },
    onError: (err) => toast.error(apiErrorText(tSettings, err)),
  });

  if (credentials.isPending) {
    return (
      <div className="flex flex-col gap-1.5 py-2" aria-hidden="true">
        <div className="h-7 w-full animate-pulse rounded-lg bg-muted motion-reduce:animate-none" />
        <div className="h-7 w-full animate-pulse rounded-lg bg-muted motion-reduce:animate-none" />
      </div>
    );
  }
  if (credentials.isError) {
    return (
      <p role="alert" className="py-2 text-xs text-destructive">
        {apiErrorText(tSettings, credentials.error)}
      </p>
    );
  }
  if (rows.length === 0) {
    return (
      <p className="py-2 text-xs text-muted-foreground">
        {t("credentials.empty")}
      </p>
    );
  }

  const credColumns: ColumnDef<ServiceCredential>[] = [
    {
      key: "name",
      header: t("credentials.columns.name"),
      cell: (row) => (
        <div className="flex flex-col">
          <span className="text-xs font-medium">{row.name}</span>
          <span className="font-mono text-2xs text-muted-foreground">
            {row.key_prefix}
          </span>
        </div>
      ),
    },
    {
      key: "scopes",
      header: t("credentials.columns.scopes"),
      cell: (row) => (
        <span className="flex max-w-xs flex-wrap gap-1">
          {row.scopes.map((s) => (
            <Badge key={s} variant="secondary" className="font-mono text-2xs">
              {s}
            </Badge>
          ))}
        </span>
      ),
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (row) => <StatusBadge status={credentialStatus(row)} />,
    },
    {
      key: "lastUsed",
      header: t("credentials.columns.lastUsed"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {row.last_used_at
            ? formatDateTime(row.last_used_at, locale)
            : t("credentials.never")}
        </span>
      ),
    },
    {
      key: "actions",
      header: "",
      cell: (row) =>
        isAdmin && !row.revoked_at ? (
          <Button type="button" variant="ghost" size="xs" onClick={() => setRevokeFor(row)}>
            {t("credentials.revoke")}
          </Button>
        ) : null,
    },
  ];

  return (
    <>
      <DataTable
        columns={credColumns}
        rows={rows}
        rowKey={(row) => row.id}
        caption={t("agents.credentials")}
        emptyState={<EmptyState title={t("credentials.empty")} compact />}
      />
      <AlertDialog open={!!revokeFor} onOpenChange={(o) => !o && setRevokeFor(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("credentials.revokeTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {revokeFor?.name} — {t("credentials.revokeDescription")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("credentials.revokeCancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={revoke.isPending}
              onClick={(e) => {
                e.preventDefault();
                if (revokeFor) {
                  revoke.mutate(revokeFor.id, {
                    onSettled: () => setRevokeFor(null),
                  });
                }
              }}
            >
              {t("credentials.revokeConfirm")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

function credentialStatus(c: ServiceCredential): "revoked" | "expired" | "active" {
  if (c.revoked_at) return "revoked";
  if (c.expires_at && new Date(c.expires_at).getTime() < Date.now()) return "expired";
  return "active";
}

// ── 생성 폼 ─────────────────────────────────────────────────

function ScopeChecklist({
  scopes,
  setScopes,
  labelPrefix,
}: {
  scopes: string[];
  setScopes: (s: string[]) => void;
  labelPrefix: string;
}) {
  return (
    <fieldset className="grid max-h-44 grid-cols-2 gap-1.5 overflow-y-auto rounded-lg border border-border p-2.5">
      <legend className="sr-only">{labelPrefix}</legend>
      {AGENT_SCOPES.map((scope) => (
        <label
          key={scope}
          htmlFor={`scope-${scope}`}
          className="flex cursor-pointer items-center gap-2 text-xs"
        >
          <Checkbox
            id={`scope-${scope}`}
            checked={scopes.includes(scope)}
            onCheckedChange={(checked) =>
              setScopes(
                checked === true
                  ? [...scopes, scope]
                  : scopes.filter((s) => s !== scope)
              )
            }
          />
          <span className="font-mono">{scope}</span>
        </label>
      ))}
    </fieldset>
  );
}

function NewAgentDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onCreated?: () => void;
}) {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const tCommon = useTranslations("common");
  const idemRef = useRef(idempotencyKey());

  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const submit = useMutation({
    mutationFn: () =>
      createAgent(
        { name: name.trim(), scopes },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("agents.createSuccess"));
      idemRef.current = idempotencyKey();
      setName("");
      setScopes([]);
      setError(null);
      onCreated?.();
      onOpenChange(false);
    },
    onError: (err) => {
      setError(apiErrorText(tSettings, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!submit.isPending && name.trim() && scopes.length > 0) submit.mutate();
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("agents.new")}</DialogTitle>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="agent-name">{t("agents.nameLabel")}</Label>
            <Input
              id="agent-name"
              required
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label>{t("agents.scopesLabel")}</Label>
            <ScopeChecklist
              scopes={scopes}
              setScopes={setScopes}
              labelPrefix={t("agents.scopesLabel")}
            />
          </div>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              {tCommon("cancel")}
            </Button>
            <Button type="submit" disabled={submit.isPending || !name.trim() || scopes.length === 0}>
              {t("agents.createSubmit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function NewCredentialButton({ agent }: { agent: Principal }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button type="button" variant="outline" size="xs" onClick={() => setOpen(true)}>
        <CredentialButtonLabel />
      </Button>
      <NewCredentialDialog agent={agent} open={open} onOpenChange={setOpen} />
    </>
  );
}

function CredentialButtonLabel() {
  const t = useTranslations("settings.members");
  return <>{t("credentials.new")}</>;
}

function NewCredentialDialog({
  agent,
  open,
  onOpenChange,
}: {
  agent: Principal;
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const idemRef = useRef(idempotencyKey());

  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>([]);
  const [expiresAt, setExpiresAt] = useState("");
  const [created, setCreated] = useState<ServiceCredentialCreated | null>(null);
  const [error, setError] = useState<string | null>(null);

  const submit = useMutation({
    mutationFn: () =>
      createAgentCredential(
        agent.id,
        {
          name: name.trim() || undefined,
          scopes,
          // datetime-local 값을 RFC 3339(UTC)로 변환해 보낸다.
          expires_at: new Date(expiresAt).toISOString(),
        },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: (res) => {
      setCreated(res.data);
      idemRef.current = idempotencyKey();
      queryClient.invalidateQueries({
        queryKey: ["settings", "agents", agent.id, "credentials"],
      });
    },
    onError: (err) => {
      setError(apiErrorText(tSettings, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  function close() {
    setCreated(null);
    setName("");
    setScopes([]);
    setExpiresAt("");
    setError(null);
    onOpenChange(false);
  }

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!submit.isPending && scopes.length > 0 && expiresAt) submit.mutate();
  }

  const secret = created ? credentialSecret(created) : undefined;

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) close();
        else onOpenChange(true);
      }}
    >
      <DialogContent>
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>{t("credentials.createdTitle")}</DialogTitle>
              <DialogDescription>{t("credentials.createdDescription")}</DialogDescription>
            </DialogHeader>
            {secret ? (
              <CopyField value={secret} label={t("credentials.columns.name")} />
            ) : (
              <p role="alert" className="text-sm text-amber-on-subtle">
                {t("inviteMissingLink")}
              </p>
            )}
            <DialogFooter>
              <Button type="button" onClick={close}>
                {tCommon("close")}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>{t("credentials.newTitle", { name: agent.name })}</DialogTitle>
            </DialogHeader>
            <form onSubmit={onSubmit} className="flex flex-col gap-3">
              <div className="grid gap-1.5">
                <Label htmlFor={`cred-name-${agent.id}`}>
                  {t("credentials.nameLabel")}
                </Label>
                <Input
                  id={`cred-name-${agent.id}`}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor={`cred-exp-${agent.id}`}>
                  {t("credentials.expiresAtLabel")}
                </Label>
                <Input
                  id={`cred-exp-${agent.id}`}
                  type="datetime-local"
                  required
                  value={expiresAt}
                  onChange={(e) => setExpiresAt(e.target.value)}
                />
              </div>
              <div className="grid gap-1.5">
                <Label>{t("agents.scopesLabel")}</Label>
                <ScopeChecklist
                  scopes={scopes}
                  setScopes={setScopes}
                  labelPrefix={t("agents.scopesLabel")}
                />
              </div>
              {error ? (
                <p role="alert" className="text-sm text-destructive">
                  {error}
                </p>
              ) : null}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={close}>
                  {tCommon("cancel")}
                </Button>
                <Button
                  type="submit"
                  disabled={submit.isPending || scopes.length === 0 || !expiresAt}
                >
                  {t("credentials.createSubmit")}
                </Button>
              </DialogFooter>
            </form>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
