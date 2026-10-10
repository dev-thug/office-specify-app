"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useLocale, useTranslations } from "next-intl";
import { useEffect, useRef, useState, type FormEvent } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
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
  getSalesCampaignSender,
  setDefaultSalesCampaignSender,
} from "@/lib/api/sales-campaigns";
import {
  createSmtpConnection,
  DELIVERY_CHANNELS_QUERY_KEY,
  getSmtpInboxStatus,
  SMTP_INBOX_QUERY_KEY,
  syncSmtpInbox,
  updateSmtpInboxSettings,
  disableSmtpConnection,
  SMTP_CONNECTIONS_QUERY_KEY,
  useSmtpConnections,
  verifySmtpConnection,
  type CreateSmtpConnectionInput,
  type SmtpInboxStatus,
  type UpdateSmtpInboxSettingsInput,
  type SmtpConnection,
  type SmtpConnectionStatus,
  type SmtpSecurity,
  type SmtpPurpose,
} from "@/lib/api/smtp";
import type { MailAccount } from "@/lib/api/settings";
import { formatDateTime } from "./format";
import { useIsAdmin } from "@/features/settings/use-me";

interface SmtpSettingsProps {
  mailAccounts: MailAccount[];
  mailAccountsLoading?: boolean;
}

interface SmtpForm {
  name: string;
  mailAccountId: string;
  purpose: SmtpPurpose;
  host: string;
  port: string;
  security: SmtpSecurity;
  username: string;
  password: string;
  fromAddress: string;
  fromName: string;
  replyTo: string;
  customHeaders: string;
  senderConfirmed: boolean;
}

const EMPTY_FORM: SmtpForm = {
  name: "",
  mailAccountId: "",
  purpose: "marketing",
  host: "",
  port: "587",
  security: "starttls",
  username: "",
  password: "",
  fromAddress: "",
  fromName: "",
  replyTo: "",
  customHeaders: "{}",
  senderConfirmed: false,
};

// Keep custom headers finite. Office owns unsubscribe and correlation headers.
const ALLOWED_CUSTOM_HEADERS = new Set([
  "x-auto-response-suppress",
  "x-importance",
]);

function parseCustomHeaders(value: string): Record<string, string> | null {
  if (!value.trim()) return {};

  try {
    const parsed: unknown = JSON.parse(value);
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      Array.isArray(parsed)
    ) {
      return null;
    }

    const entries = Object.entries(parsed);
    if (
      entries.some(
        ([key, headerValue]) =>
          !ALLOWED_CUSTOM_HEADERS.has(key.toLowerCase()) ||
          typeof headerValue !== "string" ||
          /[\r\n]/.test(headerValue) ||
          headerValue.length > 998
      )
    ) {
      return null;
    }

    return Object.fromEntries(entries);
  } catch {
    return null;
  }
}

function isPermanentApiError(error: unknown): error is ApiError {
  return (
    error instanceof ApiError &&
    error.status < 500 &&
    error.problem.retryable !== true
  );
}

function statusMessage(
  status: SmtpConnectionStatus,
  t: ReturnType<typeof useTranslations<"settings.smtp">>
) {
  switch (status) {
    case "active":
      return t("statuses.active");
    case "disabled":
      return t("statuses.disabled");
    case "reauth_required":
      return t("statuses.reauthRequired");
    default:
      return t("statuses.pending");
  }
}

function defaultImapHostForConnection(connection: SmtpConnection): string {
  return connection.host.toLowerCase().endsWith(".zoho.com")
    ? "imappro.zoho.com"
    : "";
}

function SmtpInboxSyncPanel({ connection }: { connection: SmtpConnection }) {
  const t = useTranslations("settings.smtp.inbox");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const hostId = `smtp-inbox-host-${connection.id}`;
  const enabledId = `smtp-inbox-enabled-${connection.id}`;
  const [formOverride, setFormOverride] = useState<{
    imapHost: string;
    enabled: boolean;
  } | null>(null);
  const [manualSyncRequestedAt, setManualSyncRequestedAt] = useState<number | null>(null);
  const [syncNoticeVisible, setSyncNoticeVisible] = useState(false);
  const syncNoticeTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [actionError, setActionError] = useState(false);
  const saveKey = useRef<string | null>(null);
  const syncKey = useRef<string | null>(null);
  const [syncBaseline, setSyncBaseline] = useState<{
    lastSuccess: string | null;
    lastError: string | null;
  } | null>(null);

  useEffect(
    () => () => {
      if (syncNoticeTimeout.current !== null) {
        clearTimeout(syncNoticeTimeout.current);
      }
    },
    []
  );

  const inbox = useQuery<SmtpInboxStatus>({
    queryKey: SMTP_INBOX_QUERY_KEY(connection.id),
    queryFn: async () => (await getSmtpInboxStatus(connection.id)).data,
    retry: false,
    refetchInterval: (query) => {
      const status = query.state.data;
      if (!status?.enabled) return false;
      const syncOutcomeChanged = Boolean(
        syncBaseline &&
          (status.last_success_at !== syncBaseline.lastSuccess ||
            status.last_error_code !== syncBaseline.lastError)
      );
      const waitingForManualSync =
        manualSyncRequestedAt !== null &&
        !syncOutcomeChanged &&
        Date.now() - manualSyncRequestedAt < 30_000;
      return waitingForManualSync ? 2_000 : 15_000;
    },
  });

  const serverHost = inbox.data
    ? inbox.data.imap_host || defaultImapHostForConnection(connection)
    : "";
  const imapHost = formOverride?.imapHost ?? serverHost;
  const enabled = formOverride?.enabled ?? inbox.data?.enabled ?? false;
  const dirty = Boolean(
    inbox.data &&
      formOverride &&
      (formOverride.imapHost !== serverHost ||
        formOverride.enabled !== inbox.data.enabled)
  );
  const syncOutcomeChanged = Boolean(
    inbox.data &&
      syncBaseline &&
      (inbox.data.last_success_at !== syncBaseline.lastSuccess ||
        inbox.data.last_error_code !== syncBaseline.lastError)
  );
  const showQueuedNotice = syncNoticeVisible && !syncOutcomeChanged;

  const update = useMutation({
    mutationFn: ({
      input,
      key,
    }: {
      input: UpdateSmtpInboxSettingsInput;
      key: string;
    }) =>
      updateSmtpInboxSettings(connection.id, input, {
        idempotencyKey: key,
      }),
    onSuccess: (result) => {
      saveKey.current = null;
      setActionError(false);
      setFormOverride(null);
      queryClient.setQueryData(
        SMTP_INBOX_QUERY_KEY(connection.id),
        result.data
      );
      void queryClient.invalidateQueries({
        queryKey: SMTP_INBOX_QUERY_KEY(connection.id),
      });
    },
    onError: (error) => {
      setActionError(true);
      if (isPermanentApiError(error)) saveKey.current = null;
    },
  });

  const sync = useMutation({
    mutationFn: (key: string) =>
      syncSmtpInbox(connection.id, { idempotencyKey: key }),
    onSuccess: () => {
      syncKey.current = null;
      setManualSyncRequestedAt(Date.now());
      setSyncNoticeVisible(true);
      if (syncNoticeTimeout.current !== null) {
        clearTimeout(syncNoticeTimeout.current);
      }
      syncNoticeTimeout.current = setTimeout(
        () => setSyncNoticeVisible(false),
        30_000
      );
      setActionError(false);
      void queryClient.invalidateQueries({
        queryKey: SMTP_INBOX_QUERY_KEY(connection.id),
      });
    },
    onError: (error) => {
      setActionError(true);
      setManualSyncRequestedAt(null);
      setSyncNoticeVisible(false);
      if (syncNoticeTimeout.current !== null) {
        clearTimeout(syncNoticeTimeout.current);
        syncNoticeTimeout.current = null;
      }
      if (isPermanentApiError(error)) syncKey.current = null;
    },
  });

  function changeHost(value: string) {
    setFormOverride({
      imapHost: value,
      enabled: formOverride?.enabled ?? inbox.data?.enabled ?? false,
    });
    setActionError(false);
    saveKey.current = null;
  }

  function changeEnabled(value: boolean) {
    setFormOverride({
      imapHost: formOverride?.imapHost ?? serverHost,
      enabled: value,
    });
    setActionError(false);
    saveKey.current = null;
  }

  function saveSettings() {
    const host = imapHost.trim();
    if (!host) {
      setActionError(true);
      return;
    }
    saveKey.current ??= idempotencyKey();
    update.mutate({
      input: { enabled, imap_host: host },
      key: saveKey.current,
    });
  }

  function requestSync() {
    if (!inbox.data?.enabled || connection.status !== "active") {
      return;
    }
    setSyncBaseline({
      lastSuccess: inbox.data.last_success_at,
      lastError: inbox.data.last_error_code,
    });
    setSyncNoticeVisible(false);
    if (syncNoticeTimeout.current !== null) {
      clearTimeout(syncNoticeTimeout.current);
      syncNoticeTimeout.current = null;
    }
    setManualSyncRequestedAt(Date.now());
    syncKey.current ??= idempotencyKey();
    setActionError(false);
    sync.mutate(syncKey.current);
  }

  return (
    <section
      aria-label={t("title")}
      className="basis-full rounded-xl border border-border bg-app-panel-muted p-3"
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="grid gap-1">
          <h3 className="text-sm font-medium">{t("title")}</h3>
          <p className="max-w-2xl text-xs text-muted-foreground">
            {t("credentialNotice")}
          </p>
        </div>
        {inbox.data ? (
          <Badge variant={inbox.data.enabled ? "success-subtle" : "secondary"}>
            {inbox.data.enabled ? t("enabled") : t("disabled")}
          </Badge>
        ) : null}
      </div>

      {inbox.isPending ? (
        <p role="status" className="mt-3 text-sm text-muted-foreground">
          {t("loading")}
        </p>
      ) : inbox.isError ? (
        <div role="alert" className="mt-3 flex items-center gap-2 text-sm text-destructive">
          <p>{t("loadFailed")}</p>
          <Button
            type="button"
            variant="link"
            size="xs"
            className="h-auto p-0 text-destructive"
            onClick={() => void inbox.refetch()}
          >
            {t("retry")}
          </Button>
        </div>
      ) : inbox.data ? (
        <div className="mt-3 grid gap-3">
          <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end">
            <div className="grid gap-1.5">
              <Label htmlFor={hostId}>{t("host")}</Label>
              <Input
                id={hostId}
                autoComplete="off"
                spellCheck={false}
                placeholder={t("hostPlaceholder")}
                value={imapHost}
                required
                onChange={(event) => changeHost(event.target.value)}
              />
            </div>
            <div className="flex flex-wrap items-center gap-3">
              <div className="flex items-center gap-2">
                <Checkbox
                  id={enabledId}
                  checked={enabled}
                  onCheckedChange={(checked) => changeEnabled(checked === true)}
                  aria-describedby={`${enabledId}-description`}
                />
                <Label htmlFor={enabledId} className="cursor-pointer text-sm">
                  {t("enable")}
                </Label>
              </div>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={
                  !dirty ||
                  !imapHost.trim() ||
                  update.isPending ||
                  inbox.isFetching
                }
                onClick={saveSettings}
              >
                {update.isPending ? t("saving") : t("save")}
              </Button>
            </div>
          </div>

          <p id={`${enabledId}-description`} className="text-xs text-muted-foreground">
            {t("serverSettings", {
              port: inbox.data.imap_port,
              folder: inbox.data.folder,
            })}
          </p>

          <dl className="grid gap-x-5 gap-y-1 text-xs sm:grid-cols-2 lg:grid-cols-3">
            <div className="flex gap-1.5">
              <dt className="text-muted-foreground">{t("importedMessages")}</dt>
              <dd>{inbox.data.imported_messages}</dd>
            </div>
            <div className="flex gap-1.5">
              <dt className="text-muted-foreground">{t("lastSuccess")}</dt>
              <dd>
                <time dateTime={inbox.data.last_success_at ?? undefined}>
                  {formatDateTime(inbox.data.last_success_at, locale)}
                </time>
              </dd>
            </div>
            <div className="flex gap-1.5">
              <dt className="text-muted-foreground">{t("nextSync")}</dt>
              <dd>{formatDateTime(inbox.data.next_sync_at, locale)}</dd>
            </div>
          </dl>

          <details className="rounded-lg border border-border px-3 py-2">
            <summary className="cursor-pointer text-xs font-medium">
              {t("advancedDiagnostics")}
            </summary>
            <p className="mt-2 text-2xs text-muted-foreground">
              {t("bookmarkDescription")}
            </p>
            <dl className="mt-2 grid gap-x-5 gap-y-1 text-xs sm:grid-cols-2">
              <div className="flex gap-1.5">
                <dt className="text-muted-foreground">{t("cursor")}</dt>
                <dd>
                  {inbox.data.cursor_uidvalidity ?? "—"} / {inbox.data.cursor_uid}
                </dd>
              </div>
            </dl>
          </details>

          {inbox.data.last_error_code ? (
            <p role="alert" className="text-xs text-destructive">
              {t("lastError", { code: inbox.data.last_error_code })}
            </p>
          ) : null}
          {showQueuedNotice ? (
            <p role="status" className="text-xs text-muted-foreground">
              {t("syncQueued")}
            </p>
          ) : null}
          {actionError ? (
            <p role="alert" className="text-sm text-destructive">
              {t("requestFailed")}
            </p>
          ) : null}

          <Button
            type="button"
            size="sm"
            className="w-fit"
            disabled={
              !inbox.data.enabled ||
              connection.status !== "active" ||
              sync.isPending
            }
            onClick={requestSync}
          >
            {sync.isPending ? t("syncing") : t("syncNow")}
          </Button>
        </div>
      ) : null}
    </section>
  );
}

export function SmtpSettings({
  mailAccounts,
  mailAccountsLoading = false,
}: SmtpSettingsProps) {
  const t = useTranslations("settings.smtp");
  const common = useTranslations("common");
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const connections = useSmtpConnections(isAdmin);
  const campaignSenderQuery = useQuery({
    queryKey: ["sales-campaigns", "sender"],
    enabled: isAdmin,
    queryFn: async () => (await getSalesCampaignSender()).data,
    retry: false,
  });
  const [formOpen, setFormOpen] = useState(false);
  const [form, setForm] = useState<SmtpForm>(EMPTY_FORM);
  const [formError, setFormError] = useState<string | null>(null);
  const [actionError, setActionError] = useState(false);
  const [campaignSenderError, setCampaignSenderError] = useState(false);
  const [disableFor, setDisableFor] = useState<SmtpConnection | null>(null);
  const createKey = useRef(idempotencyKey());
  const actionKeys = useRef(new Map<string, string>());

  function requestKey(action: "verify" | "disable" | "campaign-sender", id: string) {
    const key = `${action}:${id}`;
    let value = actionKeys.current.get(key);
    if (!value) {
      value = idempotencyKey();
      actionKeys.current.set(key, value);
    }
    return value;
  }

  function invalidateSettings() {
    void queryClient.invalidateQueries({
      queryKey: SMTP_CONNECTIONS_QUERY_KEY,
    });
    void queryClient.invalidateQueries({
      queryKey: DELIVERY_CHANNELS_QUERY_KEY,
    });
    void queryClient.invalidateQueries({
      queryKey: ["sales-campaigns", "sender"],
    });
  }

  function resetForm() {
    setForm(EMPTY_FORM);
    setFormError(null);
  }

  function handleFormOpenChange(open: boolean) {
    if (!open) resetForm();
    setFormOpen(open);
  }

  const create = useMutation({
    mutationFn: (input: CreateSmtpConnectionInput) =>
      createSmtpConnection(input, { idempotencyKey: createKey.current }),
    onSuccess: () => {
      createKey.current = idempotencyKey();
      resetForm();
      setFormOpen(false);
      setActionError(false);
      invalidateSettings();
    },
    onError: (error) => {
      setFormError(t("requestFailed"));
      if (isPermanentApiError(error)) createKey.current = idempotencyKey();
    },
  });

  const verify = useMutation({
    mutationFn: (id: string) =>
      verifySmtpConnection(id, {
        idempotencyKey: requestKey("verify", id),
      }),
    onSuccess: (_result, id) => {
      actionKeys.current.delete(`verify:${id}`);
      setActionError(false);
      invalidateSettings();
    },
    onError: (error, id) => {
      setActionError(true);
      if (isPermanentApiError(error)) {
        actionKeys.current.delete(`verify:${id}`);
      }
    },
  });

  const disable = useMutation({
    mutationFn: (id: string) =>
      disableSmtpConnection(id, {
        idempotencyKey: requestKey("disable", id),
      }),
    onSuccess: (_result, id) => {
      actionKeys.current.delete(`disable:${id}`);
      setDisableFor(null);
      setActionError(false);
      invalidateSettings();
    },
    onError: (error, id) => {
      setActionError(true);
      if (isPermanentApiError(error)) {
        actionKeys.current.delete(`disable:${id}`);
      }
    },
  });

  const setCampaignSender = useMutation({
    mutationFn: (id: string) =>
      setDefaultSalesCampaignSender(id, {
        idempotencyKey: requestKey("campaign-sender", id),
      }),
    onSuccess: (_result, id) => {
      actionKeys.current.delete(`campaign-sender:${id}`);
      setCampaignSenderError(false);
      invalidateSettings();
    },
    onError: (error, id) => {
      setCampaignSenderError(true);
      if (isPermanentApiError(error)) {
        actionKeys.current.delete(`campaign-sender:${id}`);
      }
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setFormError(null);

    if (!form.mailAccountId) {
      setFormError(t("mailAccountRequired"));
      return;
    }
    if (!form.senderConfirmed) {
      setFormError(t("senderConfirmationRequired"));
      return;
    }

    const port = Number(form.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setFormError(t("portInvalid"));
      return;
    }

    const customHeaders = parseCustomHeaders(form.customHeaders);
    if (!customHeaders) {
      setFormError(t("customHeadersInvalid"));
      return;
    }

    create.mutate({
      name: form.name.trim(),
      mail_account_id: form.mailAccountId,
      purpose: form.purpose,
      host: form.host.trim(),
      port,
      security: form.security,
      username: form.username.trim(),
      password: form.password,
      from_address: form.fromAddress.trim(),
      from_name: form.fromName.trim(),
      reply_to: form.replyTo.trim() || null,
      custom_headers: customHeaders,
      sender_confirmed: true,
    });
  }

  if (!isAdmin) return null;

  const statusLabel = (status: SmtpConnectionStatus) => statusMessage(status, t);
  const accountName = new Map(mailAccounts.map((account) => [account.id, account.name]));

  return (
    <>
      <Card>
        <CardHeader className="flex flex-row flex-wrap items-start justify-between gap-4">
          <div className="grid gap-1.5">
            <CardTitle>{t("title")}</CardTitle>
            <CardDescription>{t("description")}</CardDescription>
          </div>
          <Button
            type="button"
            size="sm"
            disabled={
              mailAccountsLoading ||
              mailAccounts.length === 0 ||
              connections.isPending
            }
            onClick={() => {
              setActionError(false);
              setFormError(null);
              setFormOpen(true);
            }}
          >
            {t("add")}
          </Button>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <section className="flex flex-col gap-2 rounded-2xl border border-border bg-app-panel-muted px-4 py-3">
            <div>
              <h3 className="text-sm font-medium">{t("campaignSender.title")}</h3>
              <p className="mt-1 text-2xs text-muted-foreground">
                {t("campaignSender.description")}
              </p>
            </div>
            {campaignSenderQuery.isPending ? (
              <p role="status" className="text-sm text-muted-foreground">
                {t("loading")}
              </p>
            ) : campaignSenderQuery.isError ? (
              <p role="alert" className="text-sm text-destructive">
                {t("campaignSender.loadFailed")}
              </p>
            ) : campaignSenderQuery.data?.sender?.ready ? (
              <p className="text-sm font-medium">
                {campaignSenderQuery.data.sender.profile_name} · {campaignSenderQuery.data.sender.from_name}
                {" <"}{campaignSenderQuery.data.sender.from_address}{">"}
              </p>
            ) : campaignSenderQuery.data?.sender ? (
              <p className="text-sm text-muted-foreground">
                {t("campaignSender.unready", {
                  name: campaignSenderQuery.data.sender.profile_name,
                })}
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">
                {t("campaignSender.unset")}
              </p>
            )}
            {campaignSenderError ? (
              <p role="alert" className="text-sm text-destructive">
                {t("campaignSender.saveFailed")}
              </p>
            ) : null}
          </section>
          <p className="text-2xs text-muted-foreground">{t("checkNotice")}</p>
          {mailAccounts.length === 0 && !mailAccountsLoading ? (
            <p className="text-sm text-muted-foreground">
              {t("mailAccountsRequired")}
            </p>
          ) : null}
          {connections.isPending ? (
            <p role="status" className="text-sm text-muted-foreground">
              {t("loading")}
            </p>
          ) : connections.isError ? (
            <p role="alert" className="text-sm text-destructive">
              {t("loadFailed")}
            </p>
          ) : connections.data?.length ? (
            <ul className="divide-y rounded-lg border">
              {connections.data.map((connection) => (
                <li
                  key={connection.id}
                  className="flex flex-wrap items-center justify-between gap-3 px-3 py-3"
                >
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <p className="text-sm font-medium">{connection.name}</p>
                      <Badge variant="secondary">
                        {statusLabel(connection.status)}
                      </Badge>
                      <Badge variant="outline">
                        {connection.purpose === "correspondence"
                          ? t("purposes.correspondence")
                          : t("purposes.marketing")}
                      </Badge>
                    </div>
                    <p className="mt-1 text-2xs text-muted-foreground">
                      {connection.username} → {connection.from_address}
                    </p>
                    <p className="mt-1 text-2xs text-muted-foreground">
                      {accountName.get(connection.mail_account_id) ??
                        t("unknownMailAccount")}{" "}
                      · {connection.host}:{connection.port} ·{" "}
                      {connection.security.toUpperCase()}
                    </p>
                    {connection.last_error_code ? (
                      <p className="mt-1 text-2xs text-destructive">
                        {t("lastCheckFailed")}
                      </p>
                    ) : null}
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {connection.status !== "disabled" ? (
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        disabled={verify.isPending}
                        onClick={() => {
                          setActionError(false);
                          verify.mutate(connection.id);
                        }}
                      >
                        {verify.isPending ? t("checking") : t("verify")}
                      </Button>
                    ) : null}
                    {connection.status === "active" ? (
                      <Button
                        type="button"
                        size="sm"
                        variant="ghost"
                        disabled={disable.isPending}
                        onClick={() => setDisableFor(connection)}
                      >
                        {t("disable")}
                      </Button>
                    ) : null}
                    {connection.purpose === "marketing" && connection.status === "active" ? (
                      campaignSenderQuery.data?.sender?.ready &&
                      campaignSenderQuery.data.sender.smtp_profile_id === connection.id ? (
                        <Badge variant="success-subtle">
                          {t("campaignSender.current")}
                        </Badge>
                      ) : (
                        <Button
                          type="button"
                          size="sm"
                          variant="outline"
                          disabled={setCampaignSender.isPending || campaignSenderQuery.isPending}
                          onClick={() => {
                            setCampaignSenderError(false);
                            setCampaignSender.mutate(connection.id);
                          }}
                        >
                          {setCampaignSender.isPending
                            ? t("campaignSender.saving")
                            : t("campaignSender.setDefault")}
                        </Button>
                      )
                    ) : null}
                  </div>
                  <SmtpInboxSyncPanel connection={connection} />
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-muted-foreground">{t("empty")}</p>
          )}
          {actionError ? (
            <p role="alert" className="text-sm text-destructive">
              {t("requestFailed")}
            </p>
          ) : null}
        </CardContent>
      </Card>

      <Dialog open={formOpen} onOpenChange={handleFormOpenChange}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("formTitle")}</DialogTitle>
            <DialogDescription>{t("formDescription")}</DialogDescription>
          </DialogHeader>
          <form onSubmit={submit} autoComplete="off" className="flex flex-col gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-name">{t("name")}</Label>
              <Input
                id="smtp-name"
                value={form.name}
                required
                onChange={(event) =>
                  setForm((current) => ({ ...current, name: event.target.value }))
                }
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-mail-account">{t("mailAccount")}</Label>
              <Select
                value={form.mailAccountId}
                onValueChange={(mailAccountId) =>
                  setForm((current) => ({ ...current, mailAccountId }))
                }
              >
                <SelectTrigger id="smtp-mail-account" className="w-full">
                  <SelectValue placeholder={t("chooseMailAccount")} />
                </SelectTrigger>
                <SelectContent>
                  {mailAccounts.map((account) => (
                    <SelectItem key={account.id} value={account.id}>
                      {account.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-purpose">{t("purpose")}</Label>
              <Select
                value={form.purpose}
                onValueChange={(purpose: SmtpPurpose) =>
                  setForm((current) => ({ ...current, purpose }))
                }
              >
                <SelectTrigger id="smtp-purpose" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="marketing">{t("purposes.marketing")}</SelectItem>
                  <SelectItem value="correspondence">{t("purposes.correspondence")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-host">{t("host")}</Label>
              <Input
                id="smtp-host"
                autoComplete="off"
                placeholder={t("hostPlaceholder")}
                value={form.host}
                required
                onChange={(event) =>
                  setForm((current) => ({ ...current, host: event.target.value }))
                }
              />
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label htmlFor="smtp-port">{t("port")}</Label>
                <Input
                  id="smtp-port"
                  type="number"
                  min={1}
                  max={65535}
                  step={1}
                  value={form.port}
                  required
                  onChange={(event) =>
                    setForm((current) => ({ ...current, port: event.target.value }))
                  }
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="smtp-security">{t("security")}</Label>
                <Select
                  value={form.security}
                  onValueChange={(security: SmtpSecurity) =>
                    setForm((current) => ({ ...current, security }))
                  }
                >
                  <SelectTrigger id="smtp-security" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="starttls">{t("starttls")}</SelectItem>
                    <SelectItem value="tls">{t("tls")}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-username">{t("username")}</Label>
              <Input
                id="smtp-username"
                autoComplete="username"
                placeholder={t("usernamePlaceholder")}
                value={form.username}
                required
                onChange={(event) =>
                  setForm((current) => ({ ...current, username: event.target.value }))
                }
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-password">{t("password")}</Label>
              <Input
                id="smtp-password"
                type="password"
                autoComplete="new-password"
                value={form.password}
                required
                onChange={(event) =>
                  setForm((current) => ({ ...current, password: event.target.value }))
                }
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-from-address">{t("fromAddress")}</Label>
              <Input
                id="smtp-from-address"
                type="email"
                autoComplete="email"
                placeholder={t("fromAddressPlaceholder")}
                value={form.fromAddress}
                required
                onChange={(event) =>
                  setForm((current) => ({
                    ...current,
                    fromAddress: event.target.value,
                  }))
                }
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-from-name">{t("fromName")}</Label>
              <Input
                id="smtp-from-name"
                value={form.fromName}
                required
                onChange={(event) =>
                  setForm((current) => ({ ...current, fromName: event.target.value }))
                }
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-reply-to">{t("replyTo")}</Label>
              <Input
                id="smtp-reply-to"
                type="email"
                placeholder={t("replyToOptional")}
                value={form.replyTo}
                onChange={(event) =>
                  setForm((current) => ({ ...current, replyTo: event.target.value }))
                }
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="smtp-custom-headers">{t("customHeaders")}</Label>
              <Textarea
                id="smtp-custom-headers"
                rows={3}
                spellCheck={false}
                value={form.customHeaders}
                onChange={(event) =>
                  setForm((current) => ({
                    ...current,
                    customHeaders: event.target.value,
                  }))
                }
              />
              <p className="text-2xs text-muted-foreground">
                {t("customHeadersHint")}
              </p>
            </div>
            <div className="flex items-start gap-2">
              <Checkbox
                id="smtp-sender-confirmed"
                checked={form.senderConfirmed}
                onCheckedChange={(checked) =>
                  setForm((current) => ({
                    ...current,
                    senderConfirmed: checked === true,
                  }))
                }
              />
              <Label
                htmlFor="smtp-sender-confirmed"
                className="cursor-pointer text-sm leading-5"
              >
                {t("senderConfirmed")}
              </Label>
            </div>
            {formError ? (
              <p role="alert" className="text-sm text-destructive">
                {formError}
              </p>
            ) : null}
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={create.isPending}
                onClick={() => handleFormOpenChange(false)}
              >
                {common("cancel")}
              </Button>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? t("saving") : t("save")}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <Dialog
        open={disableFor !== null}
        onOpenChange={(open) => {
          if (!open) setDisableFor(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("disableTitle")}</DialogTitle>
            <DialogDescription>
              {disableFor
                ? t("disableDescription", { name: disableFor.name })
                : null}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={disable.isPending}
              onClick={() => setDisableFor(null)}
            >
              {common("cancel")}
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={disable.isPending || disableFor === null}
              onClick={() => {
                if (disableFor) disable.mutate(disableFor.id);
              }}
            >
              {disable.isPending ? t("disabling") : t("confirmDisable")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
