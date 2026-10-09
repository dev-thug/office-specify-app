"use client";

/**
 * 설정 > 메일 계정 — 연결 상태·동기화 건강·grant 관리.
 * OAuth 비밀 값은 절대 표시하지 않고, 연결·재연결은 authorize URL 대화상자로 안내한다.
 */
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  authorizationUrl,
  createMailConnectionRequest,
  reconnectMailAccount,
  useMailAccounts,
  type MailAccount,
} from "@/lib/api/settings";
import { ConnectUrlDialog } from "@/features/operations/connect-url-dialog";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDateTime } from "@/features/settings/format";
import { GrantsSheet } from "@/features/settings/grants-sheet";
import { useIsAdmin } from "@/features/settings/use-me";

/** Zoho 데이터센터 리전 (connection_requests DDL 주석). */
const ZOHO_REGIONS = ["com", "eu", "in", "com.au", "jp", "ca", "sa", "com.cn"] as const;

export function MailAccountsScreen() {
  const t = useTranslations("settings.mailAccounts");
  const tSettings = useTranslations("settings");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();

  const accounts = useMailAccounts();
  const rows = accounts.data?.pages.flatMap((p) => p.data) ?? [];
  const page = accounts.data?.pages.at(-1)?.page;

  const [connectOpen, setConnectOpen] = useState(false);
  const [connectUrl, setConnectUrl] = useState<string | null>(null);
  const [grantsFor, setGrantsFor] = useState<MailAccount | null>(null);

  const reconnect = useMutation({
    mutationFn: (id: string) =>
      reconnectMailAccount(id, { idempotencyKey: idempotencyKey() }),
    onSuccess: (res) => {
      const url = authorizationUrl(res.data);
      if (url) setConnectUrl(url);
      else toast.success(t("reconnectDone"));
      queryClient.invalidateQueries({ queryKey: ["settings", "mail-accounts"] });
    },
    onError: (err) => toast.error(apiErrorText(tSettings, err)),
  });

  const columns: ColumnDef<MailAccount>[] = [
    {
      key: "name",
      header: t("columns.name"),
      cell: (row) => (
        <div className="flex flex-col">
          <span className="font-medium">{row.name}</span>
          <span className="text-2xs text-muted-foreground">{row.primary_address}</span>
        </div>
      ),
    },
    {
      key: "provider",
      header: t("columns.provider"),
      cell: (row) => (
        <span className="text-xs">
          {t.has(`provider.${row.provider}`) ? t(`provider.${row.provider}`) : row.provider}
        </span>
      ),
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "lastSync",
      header: t("columns.lastSync"),
      cell: (row) => (
        <div className="flex flex-col text-xs">
          <span className="whitespace-nowrap">
            {formatDateTime(row.last_synced_at, locale)}
          </span>
          {row.last_error_code ? (
            <span className="text-2xs text-destructive">{row.last_error_code}</span>
          ) : null}
        </div>
      ),
    },
    {
      key: "actions",
      header: t("columns.actions"),
      cell: (row) => (
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="xs"
            onClick={() => setGrantsFor(row)}
          >
            {t("manageGrants")}
          </Button>
          {isAdmin && row.status === "reauth_required" ? (
            <Button
              type="button"
              variant="outline"
              size="xs"
              disabled={reconnect.isPending}
              onClick={() => reconnect.mutate(row.id)}
            >
              {t("reconnect")}
            </Button>
          ) : null}
        </div>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          isAdmin ? (
            <Button type="button" size="sm" onClick={() => setConnectOpen(true)}>
              {t("connect")}
            </Button>
          ) : undefined
        }
      />
      <p className="text-2xs text-muted-foreground">{t("oauthNotice")}</p>

      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={accounts.isPending}
        error={accounts.error}
        onRetry={() => accounts.refetch()}
        page={page}
        onLoadMore={() => accounts.fetchNextPage()}
        loadingMore={accounts.isFetchingNextPage}
        emptyState={
          <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
        }
        caption={t("title")}
      />

      <ConnectDialog
        open={connectOpen}
        onOpenChange={setConnectOpen}
        onUrl={setConnectUrl}
      />
      <ConnectUrlDialog
        url={connectUrl}
        open={connectUrl !== null}
        onOpenChange={(open) => !open && setConnectUrl(null)}
      />
      <GrantsSheet
        account={grantsFor}
        open={grantsFor !== null}
        onOpenChange={(open) => !open && setGrantsFor(null)}
      />
    </div>
  );
}

function ConnectDialog({
  open,
  onOpenChange,
  onUrl,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onUrl: (url: string | null) => void;
}) {
  const t = useTranslations("settings.mailAccounts");
  const tSettings = useTranslations("settings");
  const tCommon = useTranslations("common");
  const idemRef = useRef(idempotencyKey());
  const [region, setRegion] = useState<string>("com");
  const [error, setError] = useState<string | null>(null);

  const submit = useMutation({
    mutationFn: () =>
      createMailConnectionRequest({ region }, { idempotencyKey: idemRef.current }),
    onSuccess: (res) => {
      idemRef.current = idempotencyKey();
      setError(null);
      onOpenChange(false);
      onUrl(authorizationUrl(res.data) ?? null);
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
    if (!submit.isPending) submit.mutate();
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("connectTitle")}</DialogTitle>
          <DialogDescription>{t("connectDescription")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="mail-region">{t("regionLabel")}</Label>
            <Select value={region} onValueChange={setRegion}>
              <SelectTrigger id="mail-region" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {ZOHO_REGIONS.map((r) => (
                  <SelectItem key={r} value={r}>
                    {r}
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
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              {tCommon("cancel")}
            </Button>
            <Button type="submit" disabled={submit.isPending}>
              {t("connectSubmit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
