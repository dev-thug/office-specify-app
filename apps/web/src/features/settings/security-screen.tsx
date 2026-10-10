"use client";

/**
 * 설정 > 보안 — 본인 세션 목록·해지 + 비밀번호 변경.
 * credential 원문·OAuth 토큰은 절대 표시하지 않는다 — 해지는 확인 후 POST.
 */
import { MonitorSmartphoneIcon } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
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
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  changeMyPassword,
  revokeMySession,
  useMySessions,
  type UserSession,
} from "@/lib/api/settings";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDateTime } from "@/features/settings/format";

export function SecurityScreen() {
  const t = useTranslations("settings.security");
  const tSettings = useTranslations("settings");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const [revokeFor, setRevokeFor] = useState<UserSession | null>(null);

  const sessions = useMySessions();
  const rows = sessions.data?.pages.flatMap((p) => p.data) ?? [];
  const page = sessions.data?.pages.at(-1)?.page;

  const revoke = useMutation({
    mutationFn: (id: string) =>
      revokeMySession(id, { idempotencyKey: idempotencyKey() }),
    onSuccess: () => {
      toast.success(t("sessions.revokeSuccess"));
      queryClient.invalidateQueries({ queryKey: ["settings", "me", "sessions"] });
    },
    onError: (err) => toast.error(apiErrorText(tSettings, err)),
  });

  function sessionStatus(row: UserSession): "revoked" | "expired" | "active" {
    if (row.revoked_at) return "revoked";
    if (new Date(row.expires_at).getTime() < Date.now()) return "expired";
    return "active";
  }

  const columns: ColumnDef<UserSession>[] = [
    {
      key: "device",
      header: t("sessions.columns.session"),
      cell: (row) => (
        <span className="flex items-center gap-2 text-sm">
          <MonitorSmartphoneIcon className="size-4 text-muted-foreground" aria-hidden />
          <span className="text-sm">
            {row.current
              ? t("sessions.currentSessionLabel", { time: formatDateTime(row.created_at, locale) })
              : t("sessions.otherSessionLabel", { time: formatDateTime(row.created_at, locale) })}
          </span>
        </span>
      ),
    },
    {
      key: "created",
      header: t("sessions.columns.createdAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.created_at, locale)}
        </span>
      ),
    },
    {
      key: "lastSeen",
      header: t("sessions.columns.lastSeen"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.last_seen_at, locale)}
        </span>
      ),
    },
    {
      key: "expires",
      header: t("sessions.columns.expiresAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.expires_at, locale)}
        </span>
      ),
    },
    {
      key: "status",
      header: t("sessions.columns.status"),
      cell: (row) => <StatusBadge status={sessionStatus(row)} />,
    },
    {
      key: "actions",
      header: "",
      cell: (row) =>
        !row.revoked_at && !row.current ? (
          <Button type="button" variant="ghost" size="xs" onClick={() => setRevokeFor(row)}>
            {t("sessions.revoke")}
          </Button>
        ) : null,
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />

      <section className="flex flex-col gap-3" aria-labelledby="security-sessions">
        <h2 id="security-sessions" className="text-sm font-medium">
          {t("sessions.title")}
        </h2>
        <DataTable
          columns={columns}
          rows={rows}
          rowKey={(row) => row.id}
          loading={sessions.isPending}
          error={sessions.error}
          onRetry={() => sessions.refetch()}
          page={page}
          onLoadMore={() => sessions.fetchNextPage()}
          loadingMore={sessions.isFetchingNextPage}
          emptyState={
            <EmptyState
              title={t("sessions.emptyTitle")}
              description={t("sessions.emptyDescription")}
            />
          }
          caption={t("sessions.title")}
        />
      </section>

      <PasswordCard />

      <AlertDialog open={!!revokeFor} onOpenChange={(o) => !o && setRevokeFor(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("sessions.revokeTitle")}</AlertDialogTitle>
            <AlertDialogDescription>{t("sessions.revokeDescription")}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("sessions.revokeCancel")}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={revoke.isPending}
              onClick={(e) => {
                e.preventDefault();
                if (revokeFor) {
                  revoke.mutate(revokeFor.id, { onSettled: () => setRevokeFor(null) });
                }
              }}
            >
              {t("sessions.revokeConfirm")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

/** 비밀번호 변경 — §4.5 정책(12–128자)을 폼에도 반영한다. */
function PasswordCard() {
  const t = useTranslations("settings.security.password");
  const tSettings = useTranslations("settings");
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef(idempotencyKey());

  const submit = useMutation({
    mutationFn: () =>
      changeMyPassword(
        { current_password: current, new_password: next },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("success"));
      idemRef.current = idempotencyKey();
      setCurrent("");
      setNext("");
      setError(null);
    },
    onError: (err) => {
      setError(apiErrorText(tSettings, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  const tooShort = next.length > 0 && next.length < 12;
  const valid = current.length > 0 && next.length >= 12 && next.length <= 128;

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!valid || submit.isPending) return;
    submit.mutate();
  }

  return (
    <Card aria-labelledby="security-password">
      <CardHeader>
        <CardTitle id="security-password" className="text-base">
          {t("title")}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <form onSubmit={onSubmit} className="flex max-w-sm flex-col gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="pw-current">{t("current")}</Label>
            <Input
              id="pw-current"
              type="password"
              autoComplete="current-password"
              required
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="pw-next">{t("next")}</Label>
            <Input
              id="pw-next"
              type="password"
              autoComplete="new-password"
              required
              minLength={12}
              maxLength={128}
              aria-describedby="pw-hint"
              aria-invalid={tooShort}
              value={next}
              onChange={(e) => setNext(e.target.value)}
            />
            <p id="pw-hint" className="text-2xs text-muted-foreground">
              {t("hint")}
            </p>
          </div>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
          <div>
            <Button type="submit" size="sm" disabled={!valid || submit.isPending}>
              {t("submit")}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}
