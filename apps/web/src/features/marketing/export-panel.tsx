"use client";

import { useCallback, useRef, useState, useSyncExternalStore } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  getMarketingExport,
  getMarketingExportContent,
  getCampaignsFields,
  getCampaignsTopics,
  prepareMarketingExport,
  refreshMarketingExport,
  sendMarketingExport,
  syncMarketingExport,
  useCampaignsSetup,
  type MarketingExport,
  type MarketingFieldMapping,
} from "@/lib/api/marketing";
import { useIsAdmin } from "@/features/settings/use-me";

const FROZEN_LAUNCH_STATES = new Set(["frozen", "awaiting_approval", "approved"]);
const RESUMABLE_SYNC_STATES = new Set(["PREPARING", "SYNCING", "NEEDS_REVIEW"]);

const OFFICE_UNSUBSCRIBE_VARIABLE = "compliance.unsubscribe_url";
const NATIVE_MERGE_TAG_PATTERN = /^\$\[(?:UD:)?[A-Z][A-Z0-9_]*\]\$$/;

function normalizedExportState(state: string) {
  return state.toUpperCase().replaceAll("-", "_");
}

function sendResponseProvesNoSubmission(data: MarketingExport) {
  const state = normalizedExportState(data.state);
  return (
    state === "NEEDS_REVIEW" ||
    (state === "READY" &&
      data.report?.reason === "prior_provider_optout_scan_incomplete")
  );
}

function storageKey(action: string, id: string) {
  return `office.marketing.${action}.${id}`;
}

function subscribeMarketingStorage(listener: () => void) {
  if (typeof window === "undefined") return () => {};
  window.addEventListener("storage", listener);
  window.addEventListener("office-marketing-storage", listener);
  return () => {
    window.removeEventListener("storage", listener);
    window.removeEventListener("office-marketing-storage", listener);
  };
}

function readStoredValue(key: string) {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStoredValue(key: string, value: string | null) {
  if (typeof window === "undefined") return;
  try {
    if (value === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
    window.dispatchEvent(new Event("office-marketing-storage"));
  } catch {
    // Current-page mutation state remains available if browser storage is unavailable.
  }
}

function getStableKey(action: string, id: string) {
  const storageId = storageKey(action, id);
  if (typeof window !== "undefined") {
    try {
      const previous = window.localStorage.getItem(storageId);
      if (previous) return previous;
      const next = idempotencyKey();
      window.localStorage.setItem(storageId, next);
      return next;
    } catch {
      // Keep a stable in-memory key when browser storage is unavailable.
    }
  }
  return idempotencyKey();
}

function clearStableKey(action: string, id: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(storageKey(action, id));
  } catch {
    // A missing storage API does not block a definitive rejected request.
  }
}

function hasMarker(action: string, id: string) {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(storageKey(action, id)) === "1";
  } catch {
    return false;
  }
}

function setMarker(action: string, id: string, present: boolean) {
  writeStoredValue(storageKey(action, id), present ? "1" : null);
}

function isDefiniteRejection(error: unknown) {
  return (
    error instanceof ApiError &&
    error.status >= 400 &&
    error.status < 500 &&
    error.problem.retryable !== true &&
    error.code !== "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"
  );
}

function parseFieldMap(raw: string): Record<string, MarketingFieldMapping> {
  const parsed: unknown = JSON.parse(raw);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("Field map must be a JSON object.");
  }
  for (const [variable, mapping] of Object.entries(parsed)) {
    if (
      !variable.trim() ||
      typeof mapping !== "object" ||
      mapping === null ||
      typeof (mapping as MarketingFieldMapping).contact_field !== "string" ||
      !(mapping as MarketingFieldMapping).contact_field.trim() ||
      typeof (mapping as MarketingFieldMapping).merge_tag !== "string" ||
      !NATIVE_MERGE_TAG_PATTERN.test((mapping as MarketingFieldMapping).merge_tag)
    ) {
      throw new Error("Every Office variable needs a contact field and merge tag.");
    }
  }
  return parsed as Record<string, MarketingFieldMapping>;
}

export function MarketingExportPanel({
  launchId,
  sourceStatus,
  sourceManifestHash,
  connectionId,
}: {
  launchId: string;
  sourceStatus: string;
  sourceManifestHash?: string | null;
  connectionId: string;
}) {
  const t = useTranslations("campaigns.marketingExport");
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const setup = useCampaignsSetup(isAdmin);
  const connection = setup.data?.data.connections.find(
    (candidate) => candidate.id === connectionId && candidate.status === "active"
  );
  const fieldsQuery = useQuery({
    queryKey: ["marketing", "connections", connectionId, "fields"],
    enabled: isAdmin && Boolean(connection),
    queryFn: () => getCampaignsFields(connectionId),
    retry: false,
  });
  const topicsQuery = useQuery({
    queryKey: ["marketing", "connections", connectionId, "topics"],
    enabled: isAdmin && Boolean(connection),
    queryFn: () => getCampaignsTopics(connectionId),
    retry: false,
  });
  const exportSnapshot = useCallback(
    () => readStoredValue(storageKey("export", launchId)),
    [launchId]
  );
  const exportId = useSyncExternalStore(subscribeMarketingStorage, exportSnapshot, () => null);
  const [topicId, setTopicId] = useState("");
  const [fieldMapJson, setFieldMapJson] = useState("{}");
  const [fieldMapVerified, setFieldMapVerified] = useState(false);
  const [unsubscribeContactFieldId, setUnsubscribeContactFieldId] = useState("");
  const [unsubscribeMergeTag, setUnsubscribeMergeTag] = useState("");
  const [unsubscribeMappingVerified, setUnsubscribeMappingVerified] = useState(false);
  const [fieldMapError, setFieldMapError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState("");
  const [previewOpen, setPreviewOpen] = useState(false);
  const [sendStarted, setSendStarted] = useState(false);
  const [sendOutcomeUnknown, setSendOutcomeUnknown] = useState(false);
  const prepareKey = useRef<string | null>(null);

  const exportQuery = useQuery({
    queryKey: ["marketing", "exports", exportId],
    enabled: Boolean(exportId),
    queryFn: () => getMarketingExport(exportId!),
    retry: false,
    refetchOnWindowFocus: false,
  });
  const exportData = exportQuery.data?.data;

  const contentQuery = useQuery({
    queryKey: ["marketing", "exports", exportId, "content"],
    enabled: previewOpen && Boolean(exportId),
    queryFn: () => getMarketingExportContent(exportId!),
    retry: false,
    refetchOnWindowFocus: false,
  });

  const prepare = useMutation({
    mutationFn: (input: { fieldMap: Record<string, MarketingFieldMapping> }) => {
      const key = (prepareKey.current ??= getStableKey("prepare", launchId));
      return prepareMarketingExport(
        {
          launch_id: launchId,
          topic_id: topicId.trim() || undefined,
          field_map: input.fieldMap,
        },
        key
      );
    },
    onSuccess: (result) => {
      const data = result.data;
      prepareKey.current = null;
      clearStableKey("prepare", launchId);
      writeStoredValue(storageKey("export", launchId), data.id);
      queryClient.setQueryData(["marketing", "exports", data.id], result);
      setActionError(null);
    },
    onError: (error) => {
      if (isDefiniteRejection(error)) {
        prepareKey.current = null;
        clearStableKey("prepare", launchId);
      }
      setActionError(t("prepareFailed"));
    },
  });

  const sync = useMutation({
    mutationFn: (data: MarketingExport) =>
      syncMarketingExport(data.id, getStableKey("sync", `${data.id}.${data.version}`)),
    onSuccess: (result) => {
      queryClient.setQueryData(["marketing", "exports", result.data.id], result);
      setConfirmation("");
      setActionError(null);
    },
    onError: (error) => {
      const data = sync.variables;
      if (data && isDefiniteRejection(error)) {
        clearStableKey("sync", `${data.id}.${data.version}`);
      }
      setActionError(t("syncFailed"));
    },
  });

  const refresh = useMutation({
    mutationFn: (id: string) => refreshMarketingExport(id),
    onSuccess: (result) => {
      queryClient.setQueryData(["marketing", "exports", result.data.id], result);
      setActionError(null);
    },
    onError: () => setActionError(t("refreshFailed")),
  });

  const send = useMutation({
    mutationFn: (data: MarketingExport) =>
      sendMarketingExport(
        data.id,
        { expected_export_hash: data.export_hash, confirmation: "SEND" },
        getStableKey("send", `${data.id}.${data.export_hash}`)
      ),
    onMutate: (data) => {
      setSendStarted(true);
      setMarker("send-started", `${data.id}.${data.export_hash}`, true);
    },
    onSuccess: (result) => {
      queryClient.setQueryData(["marketing", "exports", result.data.id], result);
      if (sendResponseProvesNoSubmission(result.data)) {
        const sendScope = `${result.data.id}.${result.data.export_hash}`;
        setMarker("send-started", sendScope, false);
        clearStableKey("send", sendScope);
        setSendStarted(false);
        setSendOutcomeUnknown(false);
      }
      setConfirmation("");
      setActionError(null);
    },
    onError: (error) => {
      if (isDefiniteRejection(error)) {
        const exportHash = exportData?.export_hash ?? "";
        clearStableKey("send", `${exportId}.${exportHash}`);
        setMarker("send-started", `${exportId}.${exportHash}`, false);
        setSendStarted(false);
        setActionError(t("sendRejected"));
        return;
      }
      setSendOutcomeUnknown(true);
      setActionError(null);
    },
  });

  if (!isAdmin) return null;

  const canPrepare =
    !exportId &&
    !prepare.isPending &&
    Boolean(connection) &&
    Boolean(sourceManifestHash) &&
    FROZEN_LAUNCH_STATES.has(sourceStatus);
  const hashMatches = Boolean(
    exportData &&
      sourceManifestHash &&
      exportData.source_manifest_hash === sourceManifestHash
  );
  const exportState = exportData ? normalizedExportState(exportData.state) : "";
  const writeScopeReady = connection?.write_scopes_ready === true;
  const persistedSendAttempt = Boolean(
    exportData && hasMarker("send-started", `${exportData.id}.${exportData.export_hash}`)
  );
  const sendLocked = sendStarted || persistedSendAttempt;
  const canSend = Boolean(
      exportData &&
      sourceStatus === "approved" &&
      exportState === "READY" &&
      hashMatches &&
      confirmation === "SEND" &&
      !sendOutcomeUnknown &&
      !sendLocked &&
      !send.isPending
  );

  function submitPrepare() {
    setActionError(null);
    setFieldMapError(null);
    try {
      const fieldMap = parseFieldMap(fieldMapJson);
      if (Object.prototype.hasOwnProperty.call(fieldMap, OFFICE_UNSUBSCRIBE_VARIABLE)) {
        setFieldMapError(t("reservedUnsubscribeMapping"));
        return;
      }

      const unsubscribeContactField = fieldsQuery.data?.data.find(
        (field) => field.field_id === unsubscribeContactFieldId
      );
      const mergeTag = unsubscribeMergeTag.trim();
      if (!unsubscribeContactField || !NATIVE_MERGE_TAG_PATTERN.test(mergeTag)) {
        setFieldMapError(t("unsubscribeMappingRequired"));
        return;
      }
      if (!unsubscribeMappingVerified) {
        setFieldMapError(t("verifyUnsubscribeMapping"));
        return;
      }
      if (Object.keys(fieldMap).length > 0 && !fieldMapVerified) {
        setFieldMapError(t("verifyMappings"));
        return;
      }

      prepare.mutate({
        fieldMap: {
          ...fieldMap,
          [OFFICE_UNSUBSCRIBE_VARIABLE]: {
            contact_field: unsubscribeContactField.display_name,
            merge_tag: mergeTag,
          },
        },
      });
    } catch (error) {
      setFieldMapError(error instanceof Error ? error.message : t("invalidFieldMap"));
    }
  }

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border bg-app-panel-muted/50 px-3 py-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="grid gap-1">
          <h3 className="text-sm font-semibold">{t("title")}</h3>
          <p className="text-2xs text-muted-foreground">
            {t("description", { connection: connection?.name ?? t("inactiveConnection") })}
          </p>
        </div>
        {exportData ? <Badge variant="secondary">{exportData.state}</Badge> : null}
      </div>

      {setup.isPending ? (
        <p className="text-sm text-muted-foreground" role="status">{t("checkingConnection")}</p>
      ) : setup.isError ? (
        <p className="text-sm text-destructive" role="alert">{t("setupFailed")}</p>
      ) : !connection ? (
        <Alert variant="warning">
          <AlertTitle>{t("inactiveConnection")}</AlertTitle>
          <AlertDescription>{t("connectBeforeExport")}</AlertDescription>
        </Alert>
      ) : null}

      {!exportId ? (
        <div className="grid gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor={`marketing-topic-${launchId}`}>{t("topic")}</Label>
            {topicsQuery.isError ? (
              <>
                <p className="text-2xs text-muted-foreground">{t("topicsLoadFailed")}</p>
                <Input
                  id={`marketing-topic-${launchId}`}
                  value={topicId}
                  onChange={(event) => setTopicId(event.target.value)}
                  placeholder={t("topicPlaceholder")}
                />
              </>
            ) : (
              <Select
                value={topicId || "default"}
                onValueChange={(value) => setTopicId(value === "default" ? "" : value)}
                disabled={topicsQuery.isPending || !connection}
              >
                <SelectTrigger id={`marketing-topic-${launchId}`} aria-label={t("topic")}>
                  <SelectValue placeholder={topicsQuery.isPending ? t("loadingTopics") : t("topicPlaceholder")} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="default">{t("defaultTopic")}</SelectItem>
                  {(topicsQuery.data?.data ?? []).map((topic) => (
                    <SelectItem key={topic.id} value={topic.id}>{topic.name} · {topic.id}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
          </div>
          <section className="grid gap-2 rounded-lg border border-border bg-background px-3 py-3">
            <h4 className="text-sm font-medium">{t("unsubscribeMappingTitle")}</h4>
            <div className="grid gap-1.5">
              <Label htmlFor={`marketing-unsubscribe-field-${launchId}`}>{t("unsubscribeContactField")}</Label>
              <Select
                value={unsubscribeContactFieldId || "__none__"}
                onValueChange={(value) => {
                  setUnsubscribeContactFieldId(value === "__none__" ? "" : value);
                  setUnsubscribeMappingVerified(false);
                  setFieldMapError(null);
                }}
                disabled={fieldsQuery.isPending || fieldsQuery.isError || !fieldsQuery.data?.data.length}
              >
                <SelectTrigger
                  id={`marketing-unsubscribe-field-${launchId}`}
                  aria-label={t("unsubscribeContactField")}
                >
                  <SelectValue placeholder={t("unsubscribeContactFieldPlaceholder")} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__none__">{t("unsubscribeContactFieldPlaceholder")}</SelectItem>
                  {(fieldsQuery.data?.data ?? []).map((field) => (
                    <SelectItem key={field.field_id} value={field.field_id}>
                      {field.display_name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor={`marketing-unsubscribe-merge-tag-${launchId}`}>{t("unsubscribeMergeTag")}</Label>
              <Input
                id={`marketing-unsubscribe-merge-tag-${launchId}`}
                value={unsubscribeMergeTag}
                onChange={(event) => {
                  setUnsubscribeMergeTag(event.target.value);
                  setUnsubscribeMappingVerified(false);
                  setFieldMapError(null);
                }}
                placeholder={t("unsubscribeMergeTagPlaceholder")}
                spellCheck={false}
              />
              <p className="text-2xs text-muted-foreground">{t("unsubscribeMergeTagHint")}</p>
            </div>
            <label className="flex items-start gap-2 text-2xs">
              <input
                type="checkbox"
                checked={unsubscribeMappingVerified}
                onChange={(event) => setUnsubscribeMappingVerified(event.target.checked)}
                className="mt-0.5"
              />
              <span>{t("unsubscribeMappingVerified")}</span>
            </label>
          </section>
          <details className="rounded-lg border border-border px-3 py-2">
            <summary className="cursor-pointer text-sm font-medium">{t("advanced")}</summary>
            <div className="mt-3 grid gap-2">
              <Label htmlFor={`marketing-field-map-${launchId}`}>{t("fieldMap")}</Label>
              <Textarea
                id={`marketing-field-map-${launchId}`}
                rows={5}
                value={fieldMapJson}
                onChange={(event) => { setFieldMapJson(event.target.value); setFieldMapVerified(false); }}
                spellCheck={false}
              />
              <p className="text-2xs text-muted-foreground">{t("fieldMapHint")}</p>
              <div className="grid gap-1 rounded-md bg-app-panel-muted px-2.5 py-2">
                <p className="text-2xs font-medium">{t("availableFields")}</p>
                {fieldsQuery.isPending ? <p role="status" className="text-2xs text-muted-foreground">{t("loadingFields")}</p> : null}
                {fieldsQuery.isError ? <p role="alert" className="text-2xs text-destructive">{t("fieldsLoadFailed")}</p> : null}
                {fieldsQuery.data?.data.length ? (
                  <ul className="flex flex-col gap-1">
                    {fieldsQuery.data.data.map((field) => (
                      <li key={field.field_id} className="flex flex-wrap gap-x-2 text-2xs">
                        <span>{field.display_name}</span>
                        <span className="text-muted-foreground">{field.field_id}</span>
                      </li>
                    ))}
                  </ul>
                ) : fieldsQuery.isSuccess ? <p className="text-2xs text-muted-foreground">{t("noFields")}</p> : null}
              </div>
              <label className="flex items-start gap-2 text-2xs">
                <input
                  type="checkbox"
                  checked={fieldMapVerified}
                  onChange={(event) => setFieldMapVerified(event.target.checked)}
                  className="mt-0.5"
                />
                <span>{t("mappingVerified")}</span>
              </label>
            </div>
          </details>
          {fieldMapError ? <p className="text-sm text-destructive" role="alert">{fieldMapError}</p> : null}
          <Button type="button" size="sm" disabled={!canPrepare} onClick={submitPrepare}>
            {prepare.isPending ? t("preparing") : t("prepare")}
          </Button>
        </div>
      ) : exportQuery.isPending ? (
        <p className="text-sm text-muted-foreground" role="status">{t("loadingExport")}</p>
      ) : exportQuery.isError ? (
        <div className="flex flex-wrap items-center gap-2">
          <p className="text-sm text-destructive" role="alert">{t("exportLoadFailed")}</p>
          <Button type="button" size="xs" variant="outline" onClick={() => exportQuery.refetch()}>{t("retryLoad")}</Button>
        </div>
      ) : exportData ? (
        <div className="grid gap-3">
          <dl className="grid gap-x-4 gap-y-2 text-2xs sm:grid-cols-[auto_1fr_auto_1fr]">
            <dt className="text-muted-foreground">{t("sender")}</dt>
            <dd>{exportData.sender_name} &lt;{exportData.sender_address}&gt;</dd>
            <dt className="text-muted-foreground">{t("subject")}</dt>
            <dd>{exportData.subject}</dd>
            <dt className="text-muted-foreground">{t("sourceHash")}</dt>
            <dd className="break-all font-mono">{exportData.source_manifest_hash}</dd>
            <dt className="text-muted-foreground">{t("exportHash")}</dt>
            <dd className="break-all font-mono">{exportData.export_hash}</dd>
            {exportData.provider_list_key ? <>
              <dt className="text-muted-foreground">{t("providerList")}</dt>
              <dd>{exportData.provider_list_key}</dd>
            </> : null}
            {exportData.topic_id ? <>
              <dt className="text-muted-foreground">{t("topicId")}</dt>
              <dd>{exportData.topic_id}</dd>
            </> : null}
            {exportData.provider_campaign_key ? <>
              <dt className="text-muted-foreground">{t("providerCampaign")}</dt>
              <dd>{exportData.provider_campaign_key}</dd>
            </> : null}
          </dl>

          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
            <Count label={t("recipients")} value={exportData.recipient_count} />
            <Count label={t("active")} value={exportData.active_count} />
            <Count label={t("pending")} value={exportData.pending_count} />
            <Count label={t("excluded")} value={exportData.excluded_count} />
          </div>

          <p className="text-2xs text-muted-foreground">{t("nativeUnsubscribe")}</p>
          {!hashMatches ? <p role="alert" className="text-sm text-destructive">{t("sourceChanged")}</p> : null}
          {sourceStatus !== "approved" ? <p className="text-2xs text-muted-foreground">{t("approvalRequired")}</p> : null}
          {exportState === "READY" && exportData.report?.reason === "prior_provider_optout_scan_incomplete" ? (
            <p role="status" className="text-sm text-muted-foreground">{t("priorOptOutScanProgress")}</p>
          ) : null}
          {exportState === "NEEDS_REVIEW" ? (
            <p role="status" className="text-sm text-muted-foreground">{t("sendNeedsReviewNoSubmission")}</p>
          ) : null}
          {connection && !writeScopeReady && (exportState === "PREPARED" || RESUMABLE_SYNC_STATES.has(exportState)) ? (
            <Alert variant="warning">
              <AlertTitle>{t("writeScopeTitle")}</AlertTitle>
              <AlertDescription>{t("writeScopeRequired")}</AlertDescription>
            </Alert>
          ) : null}

          <div className="flex flex-wrap gap-2">
          {exportState === "PREPARED" || RESUMABLE_SYNC_STATES.has(exportState) ? (
              <Button
                type="button"
                size="sm"
                disabled={sync.isPending || !hashMatches || !writeScopeReady}
                onClick={() => { setActionError(null); sync.mutate(exportData); }}
              >
                {sync.isPending
                  ? t("syncing")
                  : RESUMABLE_SYNC_STATES.has(exportState)
                    ? t("continueSync")
                    : t("sync")}
              </Button>
            ) : null}
            <Button type="button" size="sm" variant="outline" onClick={() => setPreviewOpen((open) => !open)}>
              {previewOpen ? t("hidePreview") : t("preview")}
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={refresh.isPending}
              onClick={() => { setActionError(null); refresh.mutate(exportData.id); }}
            >
              {refresh.isPending ? t("refreshing") : t("refresh")}
            </Button>
          </div>

          {previewOpen ? (
            <div className="grid gap-2 rounded-lg border border-border bg-background p-3">
              {contentQuery.isPending ? <p role="status" className="text-sm text-muted-foreground">{t("loadingPreview")}</p> : null}
              {contentQuery.isError ? <p role="alert" className="text-sm text-destructive">{t("previewFailed")}</p> : null}
              {contentQuery.data?.data ? <>
                <p className="text-sm font-medium">{contentQuery.data.data.subject}</p>
                {contentQuery.data.data.export_hash !== exportData.export_hash ? (
                  <p role="alert" className="text-sm text-destructive">{t("previewHashMismatch")}</p>
                ) : (
                  <iframe
                    title={t("previewFrameTitle")}
                    sandbox=""
                    srcDoc={contentQuery.data.data.html}
                    className="h-96 w-full rounded border border-border bg-background"
                  />
                )}
              </> : null}
            </div>
          ) : null}

          {exportState === "READY" ? (
            <div className="grid gap-2 rounded-lg border border-border bg-background p-3">
              <Label htmlFor={`marketing-send-confirm-${exportData.id}`}>{t("sendConfirmation")}</Label>
              <Input
                id={`marketing-send-confirm-${exportData.id}`}
                value={confirmation}
                onChange={(event) => setConfirmation(event.target.value)}
                autoComplete="off"
              />
              <Button
                type="button"
                size="sm"
                variant="destructive"
                disabled={!canSend}
                onClick={() => { setActionError(null); send.mutate(exportData); }}
              >
                {send.isPending ? t("sending") : t("send")}
              </Button>
            </div>
          ) : null}

          {sendOutcomeUnknown || (persistedSendAttempt && exportState !== "SENT") ? <p role="alert" className="text-sm text-destructive">{t("sendOutcomeUnknown")}</p> : null}
        </div>
      ) : null}

      {actionError ? (
        <Alert variant="destructive">
          <AlertTitle>{t("actionFailed")}</AlertTitle>
          <AlertDescription>{actionError}</AlertDescription>
        </Alert>
      ) : null}
    </section>
  );
}

function Count({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-md border border-border bg-background px-2.5 py-2">
      <dt className="text-2xs text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 text-sm font-semibold tabular-nums">{value}</dd>
    </div>
  );
}
