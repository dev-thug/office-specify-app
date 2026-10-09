"use client";

/**
 * 설정 > 멤버 — principals 목록, 초대(일회성 링크 1회 표시), 역할·상태 변경,
 * 비활성화 요청(202 job). Admin 전용 행동은 useIsAdmin 로 숨긴다 —
 * 숨김은 힌트이며 서버가 다시 검사한다 (§5.2).
 */
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  createMemberInvitation,
  invitationLink,
  requestPrincipalDisable,
  updatePrincipal,
  useMemberInvitations,
  usePrincipals,
  type DisablePendingWorkPolicy,
  type InvitableRole,
  type MemberInvitation,
  type MemberInvitationCreated,
  type Principal,
  type WorkspaceRole,
} from "@/lib/api/settings";
import { AgentsSection } from "@/features/settings/agents-section";
import { apiErrorText } from "@/features/settings/api-error";
import { CopyField } from "@/features/settings/copy-field";
import { formatDate, formatDateTime } from "@/features/settings/format";
import { useIsAdmin, useMe } from "@/features/settings/use-me";

const INVITABLE_ROLES: InvitableRole[] = ["member", "manager", "admin"];
const HUMAN_ROLES: WorkspaceRole[] = ["member", "manager", "admin"];

export function MembersScreen() {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const { data: me } = useMe();
  const queryClient = useQueryClient();

  const [kindFilter, setKindFilter] = useState("");
  const [inviteOpen, setInviteOpen] = useState(false);
  const [disableFor, setDisableFor] = useState<Principal | null>(null);

  const principals = usePrincipals({ kind: (kindFilter || undefined) as never });
  const rows = principals.data?.pages.flatMap((p) => p.data) ?? [];
  const page = principals.data?.pages.at(-1)?.page;

  const roleMutation = useMutation({
    mutationFn: ({ principal, role }: { principal: Principal; role: WorkspaceRole }) =>
      updatePrincipal(
        principal.id,
        { role },
        { etag: principal.version, idempotencyKey: idempotencyKey() }
      ),
    onSuccess: () => {
      toast.success(t("roleChangeSuccess"));
      queryClient.invalidateQueries({ queryKey: ["settings", "principals"] });
    },
    onError: (err) => {
      toast.error(apiErrorText(tSettings, err));
      queryClient.invalidateQueries({ queryKey: ["settings", "principals"] });
    },
  });

  const columns: ColumnDef<Principal>[] = [
    {
      key: "name",
      header: t("columns.name"),
      cell: (row) => <span className="font-medium">{row.name}</span>,
    },
    {
      key: "kind",
      header: t("columns.kind"),
      cell: (row) => (
        <Badge variant="secondary">
          {t.has(`kind.${row.kind}`) ? t(`kind.${row.kind}`) : row.kind}
        </Badge>
      ),
    },
    {
      key: "email",
      header: t("columns.email"),
      cell: (row) => <span className="text-xs">{row.email ?? "—"}</span>,
    },
    {
      key: "role",
      header: t("columns.role"),
      cell: (row) =>
        isAdmin && row.kind === "human" && row.status === "active" ? (
          <Select
            value={row.role}
            onValueChange={(role) =>
              roleMutation.mutate({ principal: row, role: role as WorkspaceRole })
            }
          >
            <SelectTrigger size="sm" aria-label={t("columns.role")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {HUMAN_ROLES.map((r) => (
                <SelectItem key={r} value={r}>
                  {t(`role.${r}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : (
          <span className="text-xs">
            {t.has(`role.${row.role}`) ? t(`role.${row.role}`) : row.role}
          </span>
        ),
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "createdAt",
      header: t("columns.createdAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDate(row.created_at, locale)}
        </span>
      ),
    },
    {
      key: "actions",
      header: t("columns.actions"),
      cell: (row) =>
        isAdmin && row.status !== "disabled" && row.id !== me?.principal_id && row.kind !== "system" ? (
          <Button
            type="button"
            variant="ghost"
            size="xs"
            onClick={() => setDisableFor(row)}
          >
            {t("disable")}
          </Button>
        ) : null,
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          isAdmin ? (
            <Button type="button" size="sm" onClick={() => setInviteOpen(true)}>
              {t("invite")}
            </Button>
          ) : undefined
        }
      />

      <section className="flex flex-col gap-3" aria-labelledby="members-table">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id="members-table" className="text-sm font-medium">
            {t("sectionMembers")}
          </h2>
          <Select value={kindFilter || "__all__"} onValueChange={(v) => setKindFilter(v === "__all__" ? "" : v)}>
            <SelectTrigger size="sm" aria-label={t("columns.kind")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__all__">{t("kindAll")}</SelectItem>
              <SelectItem value="human">{t("kind.human")}</SelectItem>
              <SelectItem value="agent">{t("kind.agent")}</SelectItem>
              <SelectItem value="system">{t("kind.system")}</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <DataTable
          columns={columns}
          rows={rows}
          rowKey={(row) => row.id}
          loading={principals.isPending}
          error={principals.error}
          onRetry={() => principals.refetch()}
          page={page}
          onLoadMore={() => principals.fetchNextPage()}
          loadingMore={principals.isFetchingNextPage}
          emptyState={
            <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
          }
          caption={t("sectionMembers")}
        />
      </section>

      <InvitationsSection />
      <AgentsSection />

      <InviteDialog open={inviteOpen} onOpenChange={setInviteOpen} />
      <DisableDialog
        principal={disableFor}
        onOpenChange={(open) => !open && setDisableFor(null)}
      />
    </div>
  );
}

// ── 초대 목록 + 일회성 링크 ─────────────────────────────────

function invitationState(inv: MemberInvitation): "accepted" | "revoked" | "expired" | "pending" {
  if (inv.accepted_at) return "accepted";
  if (inv.revoked_at) return "revoked";
  if (new Date(inv.expires_at).getTime() < Date.now()) return "expired";
  return "pending";
}

function InvitationsSection() {
  const t = useTranslations("settings.members");
  const locale = useLocale();
  const invitations = useMemberInvitations();
  const rows = invitations.data?.pages.flatMap((p) => p.data) ?? [];
  const page = invitations.data?.pages.at(-1)?.page;

  const columns: ColumnDef<MemberInvitation>[] = [
    {
      key: "email",
      header: t("invitation.email"),
      cell: (row) => <span className="text-sm">{row.email}</span>,
    },
    {
      key: "role",
      header: t("invitation.role"),
      cell: (row) => (
        <span className="text-xs">
          {t.has(`role.${row.role}`) ? t(`role.${row.role}`) : row.role}
        </span>
      ),
    },
    {
      key: "state",
      header: t("invitation.state"),
      cell: (row) => (
        <StatusBadge
          status={invitationState(row) === "pending" ? "invited" : invitationState(row)}
          label={t(`invitation.state${capitalize(invitationState(row))}`)}
        />
      ),
    },
    {
      key: "expiresAt",
      header: t("invitation.expiresAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.expires_at, locale)}
        </span>
      ),
    },
  ];

  return (
    <section className="flex flex-col gap-3" aria-labelledby="invitations-table">
      <h2 id="invitations-table" className="text-sm font-medium">
        {t("sectionInvitations")}
      </h2>
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={invitations.isPending}
        error={invitations.error}
        onRetry={() => invitations.refetch()}
        page={page}
        onLoadMore={() => invitations.fetchNextPage()}
        loadingMore={invitations.isFetchingNextPage}
        emptyState={
          <EmptyState
            title={t("invitation.emptyTitle")}
            description={t("invitation.emptyDescription")}
          />
        }
        caption={t("sectionInvitations")}
      />
    </section>
  );
}

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

// ── 초대 생성 — 링크는 응답에서 한 번만 표시된다 (X2) ─────────

function InviteDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const idemRef = useRef(idempotencyKey());

  const [email, setEmail] = useState("");
  const [role, setRole] = useState<InvitableRole>("member");
  const [created, setCreated] = useState<MemberInvitationCreated | null>(null);
  const [error, setError] = useState<string | null>(null);

  const submit = useMutation({
    mutationFn: () =>
      createMemberInvitation(
        { email: email.trim(), role },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: (res) => {
      setCreated(res.data);
      idemRef.current = idempotencyKey();
      queryClient.invalidateQueries({ queryKey: ["settings", "member-invitations"] });
      queryClient.invalidateQueries({ queryKey: ["settings", "principals"] });
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
    setEmail("");
    setRole("member");
    setError(null);
    onOpenChange(false);
  }

  const link = created ? invitationLink(created) : undefined;

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
              <DialogTitle>{t("inviteCreatedTitle")}</DialogTitle>
              <DialogDescription>{t("inviteCreatedDescription")}</DialogDescription>
            </DialogHeader>
            {link ? (
              <CopyField value={link} label={t("inviteTitle")} />
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
              <DialogTitle>{t("inviteTitle")}</DialogTitle>
            </DialogHeader>
            <InviteForm
              email={email}
              setEmail={setEmail}
              role={role}
              setRole={setRole}
              error={error}
              pending={submit.isPending}
              onSubmit={(e) => {
                e.preventDefault();
                if (!submit.isPending && email.trim()) submit.mutate();
              }}
              onCancel={close}
            />
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function InviteForm({
  email,
  setEmail,
  role,
  setRole,
  error,
  pending,
  onSubmit,
  onCancel,
}: {
  email: string;
  setEmail: (v: string) => void;
  role: InvitableRole;
  setRole: (v: InvitableRole) => void;
  error: string | null;
  pending: boolean;
  onSubmit: (e: FormEvent<HTMLFormElement>) => void;
  onCancel: () => void;
}) {
  const t = useTranslations("settings.members");
  const tCommon = useTranslations("common");
  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-3">
      <div className="grid gap-1.5">
        <Label htmlFor="invite-email">{t("inviteEmail")}</Label>
        <Input
          id="invite-email"
          type="email"
          required
          autoComplete="off"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="invite-role">{t("inviteRole")}</Label>
        <Select value={role} onValueChange={(v) => setRole(v as InvitableRole)}>
          <SelectTrigger id="invite-role" className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {INVITABLE_ROLES.map((r) => (
              <SelectItem key={r} value={r}>
                {t(`role.${r}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          {tCommon("cancel")}
        </Button>
        <Button type="submit" disabled={pending || !email.trim()}>
          {t("inviteSubmit")}
        </Button>
      </DialogFooter>
    </form>
  );
}

// ── 비활성화 요청 (202 job) ─────────────────────────────────

function DisableDialog({
  principal,
  onOpenChange,
}: {
  principal: Principal | null;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("settings.members");
  const tSettings = useTranslations("settings");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const idemRef = useRef(idempotencyKey());

  const [policy, setPolicy] = useState<DisablePendingWorkPolicy>("cancel");
  const [reassignTo, setReassignTo] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);

  const humans = usePrincipals({ kind: "human", status: "active" });
  const candidates =
    humans.data?.pages.flatMap((p) => p.data).filter((p) => p.id !== principal?.id) ?? [];

  const submit = useMutation({
    mutationFn: () =>
      requestPrincipalDisable(
        principal!.id,
        {
          pending_work_policy: policy,
          reassign_to: policy === "reassign" ? reassignTo || undefined : undefined,
          reason: reason.trim() || undefined,
        },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("disableSuccess"));
      idemRef.current = idempotencyKey();
      setError(null);
      queryClient.invalidateQueries({ queryKey: ["settings", "principals"] });
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
    if (submit.isPending) return;
    if (policy === "reassign" && !reassignTo) return;
    submit.mutate();
  }

  return (
    <Dialog open={!!principal} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("disableTitle")}</DialogTitle>
          <DialogDescription>
            {principal?.name} — {t("disableDescription")}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="disable-policy">{t("disablePolicy")}</Label>
            <Select
              value={policy}
              onValueChange={(v) => setPolicy(v as DisablePendingWorkPolicy)}
            >
              <SelectTrigger id="disable-policy" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="cancel">{t("disablePolicyCancel")}</SelectItem>
                <SelectItem value="reassign">{t("disablePolicyReassign")}</SelectItem>
              </SelectContent>
            </Select>
          </div>
          {policy === "reassign" ? (
            <div className="grid gap-1.5">
              <Label htmlFor="disable-reassign">{t("disableReassignTo")}</Label>
              <Select value={reassignTo} onValueChange={setReassignTo}>
                <SelectTrigger id="disable-reassign" className="w-full">
                  <SelectValue placeholder={t("disableReassignTo")} />
                </SelectTrigger>
                <SelectContent>
                  {candidates.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          ) : null}
          <div className="grid gap-1.5">
            <Label htmlFor="disable-reason">{t("disableReason")}</Label>
            <Textarea
              id="disable-reason"
              rows={2}
              value={reason}
              onChange={(e) => setReason(e.target.value)}
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
            <Button
              type="submit"
              variant="destructive"
              disabled={submit.isPending || (policy === "reassign" && !reassignTo)}
            >
              {t("disableSubmit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
