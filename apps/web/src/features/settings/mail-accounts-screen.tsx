"use client";

/**
 * 설정 > 메일 계정 — 연결 상태·동기화 건강·grant 관리.
 * OAuth 비밀 값은 절대 표시하지 않고, 연결·재연결은 authorize URL 대화상자로 안내한다.
 */
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
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
import { Input } from "@/components/ui/input";
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
  ZOHO_OAUTH_SETTINGS_KEY,
  authorizationUrl,
  createMailConnectionRequest,
  getZohoOAuthSettings,
  putZohoOAuthSettings,
  reconnectMailAccount,
  useMailAccounts,
  type MailAccount,
} from "@/lib/api/settings";
import { SmtpSettings } from "@/features/settings/smtp-settings";
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

      <SmtpSettings mailAccounts={rows} mailAccountsLoading={accounts.isPending} />

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
  onOpenChange: (open: boolean) => void;
  onUrl: (url: string | null) => void;
}) {
  const t = useTranslations("settings.mailAccounts");
  const tSettings = useTranslations("settings");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const idemRef = useRef(idempotencyKey());
  const [region, setRegion] = useState<string>("com");
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [redirectUri, setRedirectUri] = useState("");
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});

  const settings = useQuery({
    queryKey: ZOHO_OAUTH_SETTINGS_KEY,
    queryFn: getZohoOAuthSettings,
    enabled: open,
  });
  const current = settings.data?.data;
  // 미설정이면 입력란을 바로 보여주고, 설정돼 있으면 요약 + 변경 버튼만 둔다.
  const showFields = editing || (current !== undefined && !current.configured);
  const hasStoredSecret = current?.has_secret === true;

  function startEditing() {
    setClientId(current?.client_id ?? "");
    setClientSecret("");
    setRedirectUri(current?.source === "workspace" ? (current.redirect_uri ?? "") : "");
    setFieldErrors({});
    setEditing(true);
  }

  function resetFields() {
    setEditing(false);
    setClientSecret("");
    setFieldErrors({});
    setError(null);
  }

  function validateFields(): boolean {
    const next: Record<string, string> = {};
    const id = clientId.trim();
    if (!id) next.client_id = t("oauthRequired");
    else if (!id.startsWith("1000.")) next.client_id = t("oauthClientIdFormat");
    if (!clientSecret.trim() && !hasStoredSecret) next.client_secret = t("oauthRequired");
    setFieldErrors(next);
    return Object.keys(next).length === 0;
  }

  const submit = useMutation({
    mutationFn: async () => {
      if (showFields) {
        await putZohoOAuthSettings(
          {
            client_id: clientId.trim(),
            ...(clientSecret.trim() ? { client_secret: clientSecret.trim() } : {}),
            ...(redirectUri.trim() ? { redirect_uri: redirectUri.trim() } : {}),
          },
          { idempotencyKey: idempotencyKey() }
        );
        queryClient.invalidateQueries({ queryKey: ZOHO_OAUTH_SETTINGS_KEY });
      }
      return createMailConnectionRequest(
        { region },
        { idempotencyKey: idemRef.current }
      );
    },
    onSuccess: (res) => {
      idemRef.current = idempotencyKey();
      setError(null);
      setClientSecret("");
      setEditing(false);
      onOpenChange(false);
      onUrl(authorizationUrl(res.data) ?? null);
    },
    onError: (err) => {
      const next: Record<string, string> = {};
      if (err instanceof ApiError) {
        for (const v of err.problem.violations) {
          if (v.field && ["client_id", "client_secret", "redirect_uri"].includes(v.field)) {
            next[v.field] = v.code === "required" ? t("oauthRequired") : t("oauthInvalid");
          }
        }
      }
      setFieldErrors(next);
      setError(Object.keys(next).length > 0 ? null : apiErrorText(tSettings, err));
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (submit.isPending) return;
    if (showFields && !validateFields()) return;
    submit.mutate();
  }

  const callbackUri = current?.default_redirect_uri;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) resetFields();
        onOpenChange(next);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("connectTitle")}</DialogTitle>
          <DialogDescription>{t("connectDescription")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-3" noValidate>
          {settings.isError ? (
            <p role="alert" className="text-sm text-destructive">
              {t("oauthLoadError")}
            </p>
          ) : null}

          {current && !showFields ? (
            <div className="flex flex-col gap-2 rounded-xl border bg-muted/40 p-3 text-sm">
              <p>
                {current.source === "environment"
                  ? t("oauthConfiguredEnv")
                  : t("oauthConfigured")}
              </p>
              <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-2xs text-muted-foreground">
                <dt>{t("clientIdLabel")}</dt>
                <dd className="break-all font-mono">{current.client_id}</dd>
                <dt>{t("redirectUriLabel")}</dt>
                <dd className="break-all font-mono">{current.redirect_uri}</dd>
              </dl>
              {current.source !== "environment" ? (
                <Button
                  type="button"
                  variant="ghost"
                  size="xs"
                  className="self-start"
                  onClick={startEditing}
                >
                  {t("oauthChange")}
                </Button>
              ) : null}
            </div>
          ) : null}

          {showFields ? (
            <fieldset className="flex flex-col gap-3 rounded-xl border p-3">
              <legend className="px-1 text-xs font-medium">{t("oauthClientTitle")}</legend>
              <p className="text-2xs text-muted-foreground">{t("oauthClientDescription")}</p>
              <div className="grid gap-1.5">
                <Label htmlFor="zoho-client-id">{t("clientIdLabel")}</Label>
                <Input
                  id="zoho-client-id"
                  value={clientId}
                  onChange={(e) => setClientId(e.target.value)}
                  placeholder={t("clientIdPlaceholder")}
                  autoComplete="off"
                  spellCheck={false}
                  aria-invalid={fieldErrors.client_id ? true : undefined}
                />
                {fieldErrors.client_id ? (
                  <p role="alert" className="text-2xs text-destructive">
                    {fieldErrors.client_id}
                  </p>
                ) : null}
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="zoho-client-secret">{t("clientSecretLabel")}</Label>
                <Input
                  id="zoho-client-secret"
                  type="password"
                  value={clientSecret}
                  onChange={(e) => setClientSecret(e.target.value)}
                  placeholder={
                    hasStoredSecret ? t("clientSecretKeep") : t("clientSecretPlaceholder")
                  }
                  autoComplete="new-password"
                  spellCheck={false}
                  aria-invalid={fieldErrors.client_secret ? true : undefined}
                />
                {fieldErrors.client_secret ? (
                  <p role="alert" className="text-2xs text-destructive">
                    {fieldErrors.client_secret}
                  </p>
                ) : null}
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="zoho-redirect-uri">{t("redirectUriLabel")}</Label>
                <Input
                  id="zoho-redirect-uri"
                  value={redirectUri}
                  onChange={(e) => setRedirectUri(e.target.value)}
                  placeholder={callbackUri}
                  autoComplete="off"
                  spellCheck={false}
                  aria-invalid={fieldErrors.redirect_uri ? true : undefined}
                />
                {fieldErrors.redirect_uri ? (
                  <p role="alert" className="text-2xs text-destructive">
                    {fieldErrors.redirect_uri}
                  </p>
                ) : null}
                <p className="text-2xs text-muted-foreground">{t("redirectUriHint")}</p>
                {callbackUri ? (
                  <div className="flex items-center gap-2">
                    <code className="min-w-0 flex-1 break-all rounded-md bg-muted px-2 py-1 text-2xs">
                      {redirectUri.trim() || callbackUri}
                    </code>
                    <Button
                      type="button"
                      variant="outline"
                      size="xs"
                      onClick={() => {
                        void navigator.clipboard
                          ?.writeText(redirectUri.trim() || callbackUri)
                          .then(() => toast.success(t("redirectUriCopied")))
                          .catch(() => undefined);
                      }}
                    >
                      {t("redirectUriCopy")}
                    </Button>
                  </div>
                ) : null}
              </div>
              {current?.configured ? (
                <p className="text-2xs text-muted-foreground">{t("oauthChangeNotice")}</p>
              ) : null}
              {editing && current?.configured ? (
                <Button
                  type="button"
                  variant="ghost"
                  size="xs"
                  className="self-start"
                  onClick={resetFields}
                >
                  {t("oauthChangeCancel")}
                </Button>
              ) : null}
            </fieldset>
          ) : null}

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
            <Button type="submit" disabled={submit.isPending || settings.isPending}>
              {showFields ? t("oauthSaveAndStart") : t("connectSubmit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
