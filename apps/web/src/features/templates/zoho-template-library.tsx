"use client";

import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useMemo, useRef, useState, type FormEvent } from "react";
import {
  CopyIcon,
  PlusIcon,
  RefreshCwIcon,
  TriangleAlertIcon,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
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
import { useMe } from "@/features/settings/use-me";
import { useCampaignsSetup } from "@/lib/api/marketing";
import { ApiError } from "@/lib/api/problem";
import { idempotencyKey } from "@/lib/api/client";
import { readEtag } from "@/lib/api/mutate";
import {
  createZohoEmailTemplate,
  deleteZohoEmailTemplate,
  getZohoEmailTemplate,
  listZohoEmailTemplates,
  mapZohoMergeFields,
  updateZohoEmailTemplate,
  zohoMergeFields,
  type ZohoEmailTemplate,
  type ZohoEmailTemplateContentType,
  type ZohoEmailTemplateSummary,
  type ZohoEmailTemplateWriteInput,
} from "@/lib/api/zoho-templates";

import { EmailCanvas } from "./email-canvas";
import {
  cleanEmailHtml,
  DEFAULT_EMAIL_VARIABLES,
  emailHtmlToText,
  escapeEmailText,
} from "./email-authoring";
import { createZohoNativePreviewDocument } from "./zoho-native-preview";

const PAGE_SIZE = 100;
const VARIABLE_NAME = /^[A-Za-z_][A-Za-z0-9_.]*$/;

function stableIdempotencyKey(keys: Map<string, string>, fingerprint: string): string {
  const existing = keys.get(fingerprint);
  if (existing) return existing;
  const key = idempotencyKey();
  keys.set(fingerprint, key);
  return key;
}

export interface ZohoOfficeImportSeed {
  subject: string;
  bodyText: string;
  bodyHtml: string | null;
}

interface RemoteTemplateEditor {
  mode: "create" | "edit";
  templateId?: string;
  etag?: string;
}

export function ZohoTemplateLibrary({
  open,
  onOpenChange,
  onImportSeed,
  allowImport = true,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onImportSeed: (seed: ZohoOfficeImportSeed) => void;
  allowImport?: boolean;
}) {
  const t = useTranslations("templates.zoho");
  const common = useTranslations("common");
  const me = useMe();
  const currentUser = me.data;
  const admin = currentUser?.kind === "human" && currentUser.role === "admin";
  const setup = useCampaignsSetup(admin && open && Boolean(currentUser?.principal_id) && Boolean(currentUser?.workspace_id));
  const queryClient = useQueryClient();
  const setupData = setup.data?.data;
  const setupWorkspaceId = setupData?.workspace_id;
  const workspaceMatches = Boolean(currentUser?.workspace_id && setupWorkspaceId && currentUser.workspace_id === setupWorkspaceId);
  const configured = workspaceMatches && setupData?.email_api_configured === true;
  const enabled = admin && open && configured;
  const cacheScope = [
    "zoho-email-api-templates",
    currentUser?.principal_id ?? "unverified-principal",
    currentUser?.workspace_id ?? "unverified-workspace",
    setupWorkspaceId ?? "unbound-workspace",
  ] as const;
  const listQueryKey = [...cacheScope, "list"] as const;

  const library = useInfiniteQuery({
    queryKey: listQueryKey,
    enabled,
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      listZohoEmailTemplates({ cursor: pageParam, limit: PAGE_SIZE }),
    getNextPageParam: (lastPage) =>
      lastPage.page?.has_more ? lastPage.page.next_cursor ?? undefined : undefined,
  });
  const templates = useMemo(
    () => library.data?.pages.flatMap((page) => page.data) ?? [],
    [library.data]
  );
  const [selectedTemplate, setSelectedTemplate] = useState<{
    principalId: string;
    workspaceId: string;
    id: string;
  } | null>(null);
  const [editor, setEditor] = useState<RemoteTemplateEditor | null>(null);
  const [name, setName] = useState("");
  const [contentType, setContentType] = useState<ZohoEmailTemplateContentType>("html");
  const [content, setContent] = useState("");
  const [nativeSourceUnsupported, setNativeSourceUnsupported] = useState(false);
  const [visualConversionConfirmed, setVisualConversionConfirmed] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [importSubject, setImportSubject] = useState("");
  const [mappings, setMappings] = useState<Record<string, string>>({});
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [unknownOutcome, setUnknownOutcome] = useState(false);
  const [reconciliationReady, setReconciliationReady] = useState(false);
  const [unknownFingerprint, setUnknownFingerprint] = useState<string | null>(null);
  const editorOperationKeys = useRef(new Map<string, string>());
  const deleteOperationKeys = useRef(new Map<string, string>());

  const selectedId = selectedTemplate
    && selectedTemplate.principalId === currentUser?.principal_id
    && selectedTemplate.workspaceId === currentUser?.workspace_id
    ? selectedTemplate.id
    : templates[0]?.id ?? null;
  function setSelectedId(id: string | null) {
    setSelectedTemplate(id && currentUser?.principal_id && currentUser.workspace_id
      ? { principalId: currentUser.principal_id, workspaceId: currentUser.workspace_id, id }
      : null);
  }
  const detailQueryKey = [...cacheScope, "detail", selectedId] as const;

  const selectedQuery = useQuery({
    queryKey: detailQueryKey,
    enabled: enabled && selectedId !== null,
    queryFn: () => getZohoEmailTemplate(selectedId!),
  });
  const selected = selectedQuery.data?.data ?? null;
  const selectedEtag = selectedQuery.data ? readEtag(selectedQuery.data) : undefined;
  const selectedCompatible = useMemo(() => {
    if (!selected || selected.content_type !== "html") return null;
    return cleanEmailHtml(selected.content);
  }, [selected]);
  const mergeFields = useMemo(
    () => (selected ? zohoMergeFields(selected.content) : []),
    [selected]
  );
  const resolvedMappings = useMemo(() =>
    Object.fromEntries(Object.entries(mappings).map(([field, mapping]) => [
      field,
      mapping.startsWith("custom:") ? mapping.slice("custom:".length).trim() : mapping,
    ])), [mappings]);
  const unresolvedFields = mergeFields.filter((field) => !mappings[field]);
  const invalidCustomFields = mergeFields.filter((field) => {
    const mapping = mappings[field];
    return Boolean(mapping?.startsWith("custom:") && !VARIABLE_NAME.test(mapping.slice("custom:".length).trim()));
  });
  const nativePreviewDocument = useMemo(() => {
    if (!selected) return "";
    const previewSource = selected.content_type === "html"
      ? selected.content
      : `<pre>${escapeEmailText(selected.content)}</pre>`;
    return createZohoNativePreviewDocument(previewSource);
  }, [selected]);
  const rawNativeSourceMode = contentType === "html" && nativeSourceUnsupported && !visualConversionConfirmed;
  const nativeEditorPreview = useMemo(
    () => rawNativeSourceMode ? createZohoNativePreviewDocument(content) : "",
    [rawNativeSourceMode, content]
  );

  function beginEditor(mode: "create" | "edit", source?: ZohoEmailTemplate) {
    if (unknownOutcome) return;
    setMutationError(null);
    setUnknownOutcome(false);
    setReconciliationReady(false);
    setImportOpen(false);
    setDeleteOpen(false);
    setName(source ? mode === "create" ? t("copyName", { name: source.name }) : source.name : "");
    setContentType(source?.content_type ?? "html");
    setContent(source?.content ?? "");
    setNativeSourceUnsupported(Boolean(source?.content_type === "html" && cleanEmailHtml(source.content).removed));
    setVisualConversionConfirmed(false);
    setEditor({
      mode,
      ...(mode === "edit" && source ? { templateId: source.id, etag: selectedEtag ?? source.content_hash } : {}),
    });
  }

  function convertNativeSourceForVisualEditing() {
    setContent(cleanEmailHtml(content).html);
    setVisualConversionConfirmed(true);
  }

  async function refreshAfterUnknown() {
    const refresh = await library.refetch();
    if (!refresh.isSuccess) return;
    if (selectedId) await queryClient.invalidateQueries({ queryKey: [...cacheScope, "detail", selectedId] });
    setReconciliationReady(true);
  }

  function isUnknownProviderResult(error: unknown): boolean {
    return error instanceof ApiError && error.code === "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN";
  }

  function acknowledgeReconciliation() {
    if (unknownFingerprint) {
      editorOperationKeys.current.delete(unknownFingerprint);
      deleteOperationKeys.current.delete(unknownFingerprint);
    }
    setUnknownFingerprint(null);
    setUnknownOutcome(false);
    setReconciliationReady(false);
    setMutationError(null);
  }

  async function submitEditor(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!editor || unknownOutcome || pending) return;
    setPending(true);
    setMutationError(null);
    const input: ZohoEmailTemplateWriteInput = {
      name: name.trim(),
      content,
      content_type: contentType,
    };
    const fingerprint = JSON.stringify({
      method: editor.mode === "create" ? "POST" : "PUT",
      principal_id: currentUser?.principal_id,
      workspace_id: currentUser?.workspace_id,
      zoho_workspace_id: setupWorkspaceId,
      template_id: editor.templateId ?? null,
      etag: editor.etag ?? null,
      input,
    });
    const operationKey = stableIdempotencyKey(editorOperationKeys.current, fingerprint);
    try {
      const result = editor.mode === "create"
        ? await createZohoEmailTemplate(input, { idempotencyKey: operationKey })
        : await updateZohoEmailTemplate(editor.templateId!, input, {
          etag: editor.etag,
          idempotencyKey: operationKey,
        });
      editorOperationKeys.current.delete(fingerprint);
      setEditor(null);
      setSelectedId(result.data.template_id ?? editor.templateId ?? null);
      await library.refetch();
      if (result.data.template_id) {
        await queryClient.invalidateQueries({ queryKey: [...cacheScope, "detail", result.data.template_id] });
      }
    } catch (error) {
      if (isUnknownProviderResult(error)) {
        setEditor(null);
        setUnknownOutcome(true);
        setReconciliationReady(false);
        setUnknownFingerprint(fingerprint);
        setMutationError(t("unknownOutcome"));
        void library.refetch();
        void queryClient.invalidateQueries({ queryKey: [...cacheScope, "detail", selectedId] });
      } else if (error instanceof ApiError && error.status === 412) {
        setEditor(null);
        setMutationError(t("staleTemplate"));
        if (editor.templateId) void queryClient.invalidateQueries({ queryKey: [...cacheScope, "detail", editor.templateId] });
      } else {
        setMutationError(t("saveFailed"));
      }
    } finally {
      setPending(false);
    }
  }

  async function confirmDelete() {
    if (!selected || unknownOutcome || pending) return;
    setPending(true);
    setMutationError(null);
    const etag = selectedEtag ?? selected.content_hash;
    const fingerprint = JSON.stringify({
      method: "DELETE",
      principal_id: currentUser?.principal_id,
      workspace_id: currentUser?.workspace_id,
      zoho_workspace_id: setupWorkspaceId,
      template_id: selected.id,
      etag,
    });
    const operationKey = stableIdempotencyKey(deleteOperationKeys.current, fingerprint);
    try {
      await deleteZohoEmailTemplate(selected.id, {
        etag,
        idempotencyKey: operationKey,
      });
      deleteOperationKeys.current.delete(fingerprint);
      setDeleteOpen(false);
      setSelectedId(null);
      await library.refetch();
    } catch (error) {
      setDeleteOpen(false);
      if (isUnknownProviderResult(error)) {
        setUnknownOutcome(true);
        setReconciliationReady(false);
        setUnknownFingerprint(fingerprint);
        setMutationError(t("unknownOutcome"));
        void library.refetch();
        void queryClient.invalidateQueries({ queryKey: [...cacheScope, "detail", selected.id] });
      } else if (error instanceof ApiError && error.status === 412) {
        setMutationError(t("staleTemplate"));
        void queryClient.invalidateQueries({ queryKey: [...cacheScope, "detail", selected.id] });
      } else {
        setMutationError(t("deleteFailed"));
      }
    } finally {
      setPending(false);
    }
  }

  function startImport() {
    if (!selected || unknownOutcome || !allowImport) return;
    setImportSubject(selected.name);
    setMappings({});
    setMutationError(null);
    setImportOpen(true);
  }

  function continueImport() {
    if (!selected || unresolvedFields.length > 0 || invalidCustomFields.length > 0) return;
    const mapped = mapZohoMergeFields(selected.content, resolvedMappings);
    if (mapped.unresolved.length > 0) return;
    const compatibleContent = selected.content_type === "html"
      ? cleanEmailHtml(mapped.content).html
      : mapped.content;
    const html = selected.content_type === "html" ? compatibleContent : null;
    const bodyText = selected.content_type === "html" ? emailHtmlToText(compatibleContent) : compatibleContent;
    onImportSeed({ subject: importSubject.trim(), bodyText, bodyHtml: html });
    setImportOpen(false);
    onOpenChange(false);
  }

  if (!admin) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[92dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-6xl">
        <DialogHeader className="shrink-0 border-b px-6 py-4">
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>

        {setup.isPending ? (
          <p className="px-6 py-8 text-sm text-muted-foreground" aria-busy="true">{t("loading")}</p>
        ) : setup.isError ? (
          <div className="p-6">
            <Alert variant="destructive"><AlertTitle>{t("setupFailed")}</AlertTitle><AlertDescription>{t("setupRetry")}</AlertDescription></Alert>
            <Button className="mt-4" type="button" variant="outline" onClick={() => void setup.refetch()}>{t("retry")}</Button>
          </div>
        ) : setup.data && !workspaceMatches ? (
          <div className="p-6">
            <Alert variant="warning"><AlertTitle>{t("workspaceMismatchTitle")}</AlertTitle><AlertDescription>{t("workspaceMismatchDescription")}</AlertDescription></Alert>
          </div>
        ) : !configured ? (
          <div className="p-6">
            <Alert variant="warning">
              <TriangleAlertIcon />
              <AlertTitle>{t("separateProductTitle")}</AlertTitle>
              <AlertDescription>
                <p>{t("separateProductDescription")}</p>
                <ul className="mt-2 list-disc space-y-1 pl-5">
                  <li><code>ZOHO_EMAIL_API_ENABLED</code></li>
                  <li><code>ZOHO_EMAIL_API_KEY</code></li>
                  <li><code>ZOHO_EMAIL_API_WORKSPACE_ID</code></li>
                </ul>
                <p className="mt-2">{t("oauthSeparation")}</p>
              </AlertDescription>
            </Alert>
          </div>
        ) : (
          <div className="grid min-h-0 flex-1 gap-0 overflow-hidden md:grid-cols-[16rem_minmax(0,1fr)]">
            <aside className="flex min-h-0 flex-col border-b p-4 md:border-r md:border-b-0">
              <div className="flex items-center justify-between gap-2">
                <h2 className="text-sm font-medium">{t("listTitle")}</h2>
                <Button type="button" variant="ghost" size="icon-sm" aria-label={t("refresh")} disabled={library.isFetching} onClick={() => void library.refetch()}>
                  <RefreshCwIcon aria-hidden />
                </Button>
              </div>
              <Button className="mt-3 justify-start" type="button" size="sm" disabled={unknownOutcome} onClick={() => beginEditor("create")}>
                <PlusIcon aria-hidden />{t("create")}
              </Button>
              <div className="mt-3 min-h-0 flex-1 overflow-y-auto">
                {library.isPending ? <p className="py-4 text-sm text-muted-foreground">{t("loading")}</p> : null}
                {library.isError ? <div className="py-3"><p className="text-sm text-destructive">{t("listFailed")}</p><Button className="mt-2" size="sm" variant="outline" onClick={() => void library.refetch()}>{t("retry")}</Button></div> : null}
                {library.isSuccess && templates.length === 0 ? <p className="py-4 text-sm text-muted-foreground">{t("empty")}</p> : null}
                <ul className="flex flex-col gap-1">
                  {templates.map((template: ZohoEmailTemplateSummary) => (
                    <li key={template.id}>
                      <button type="button" aria-current={selectedId === template.id ? "page" : undefined} onClick={() => { setSelectedId(template.id); setEditor(null); setImportOpen(false); setMutationError(null); }} className={`w-full rounded-lg px-3 py-2 text-left text-sm ${selectedId === template.id ? "bg-accent font-medium text-foreground" : "text-muted-foreground hover:bg-muted/60 hover:text-foreground"}`}>
                        <span className="block truncate">{template.name}</span>
                      </button>
                    </li>
                  ))}
                </ul>
                {library.hasNextPage ? <Button className="mt-3 w-full" type="button" variant="outline" size="sm" disabled={library.isFetchingNextPage} onClick={() => void library.fetchNextPage()}>{t("loadMore")}</Button> : null}
              </div>
            </aside>

            <div className="flex min-h-0 flex-col overflow-y-auto p-5 md:p-6">
              {unknownOutcome ? (
                <Alert variant="warning">
                  <TriangleAlertIcon />
                  <AlertTitle>{t("unknownTitle")}</AlertTitle>
                  <AlertDescription>
                    <p>{mutationError ?? t("unknownOutcome")}</p>
                    <Button className="mt-3" type="button" size="sm" variant="outline" onClick={() => void refreshAfterUnknown()}>{t("refreshBeforeRetry")}</Button>
                    {reconciliationReady ? <Button className="mt-3 ml-2" type="button" size="sm" variant="outline" onClick={acknowledgeReconciliation}>{t("acknowledgeReconciliation")}</Button> : null}
                  </AlertDescription>
                </Alert>
              ) : null}
              {!unknownOutcome && mutationError ? <Alert variant="destructive"><AlertTitle>{t("actionFailed")}</AlertTitle><AlertDescription>{mutationError}</AlertDescription></Alert> : null}

              {editor ? (
                <form onSubmit={submitEditor} className="flex flex-col gap-4">
                  <div>
                    <h2 className="text-base font-semibold">{editor.mode === "create" ? t("createTitle") : t("editTitle")}</h2>
                    <p className="mt-1 text-sm text-muted-foreground">{t("editorHint")}</p>
                  </div>
                  <div className="grid gap-1.5">
                    <Label htmlFor="zoho-template-name">{t("nameLabel")}</Label>
                    <Input id="zoho-template-name" aria-label={t("nameLabel")} required maxLength={300} value={name} onChange={(event) => setName(event.target.value)} />
                  </div>
                  <div className="grid max-w-xs gap-1.5">
                    <Label htmlFor="zoho-template-content-type">{t("contentType")}</Label>
                    <select id="zoho-template-content-type" className="h-9 rounded-lg border border-input bg-background px-3 text-sm" value={contentType} onChange={(event) => setContentType(event.target.value as ZohoEmailTemplateContentType)}>
                      <option value="html">HTML</option><option value="text">Text</option>
                    </select>
                  </div>
                  <div className="grid gap-1.5">
                    {contentType === "html" ? (
                      rawNativeSourceMode ? (
                        <div className="flex flex-col gap-3">
                          <Alert variant="warning">
                            <TriangleAlertIcon />
                            <AlertTitle>{t("nativeEditTitle")}</AlertTitle>
                            <AlertDescription>
                              <p>{t("nativeEditDescription")}</p>
                              <Button className="mt-3" type="button" size="sm" variant="outline" onClick={convertNativeSourceForVisualEditing}>{t("convertForVisualEditing")}</Button>
                            </AlertDescription>
                          </Alert>
                          <div className="grid gap-1.5">
                            <Label htmlFor="zoho-template-content">{t("contentLabel")}</Label>
                            <Textarea id="zoho-template-content" aria-label={t("contentLabel")} value={content} onChange={(event) => setContent(event.target.value)} rows={14} className="font-mono text-ui" spellCheck={false} />
                          </div>
                          <div className="overflow-hidden rounded-xl border">
                            <p className="border-b bg-muted/40 px-4 py-2 text-xs font-medium">{t("nativeEditorPreviewTitle")}</p>
                            <p className="border-b px-4 py-2 text-2xs text-muted-foreground">{t("nativePreviewNetworkNotice")}</p>
                            <iframe title={t("nativeEditorPreviewTitle")} sandbox="" referrerPolicy="no-referrer" srcDoc={nativeEditorPreview} className="h-80 w-full bg-background" />
                          </div>
                        </div>
                      ) : (
                        <>
                          <p className="text-sm font-medium">{t("contentLabel")}</p>
                          <EmailCanvas html={content} previewHtml={selectedPreviewHtml(content)} previewSubject={name} onChange={setContent} onFocus={() => undefined} />
                        </>
                      )
                    ) : (
                      <div className="grid gap-1.5"><Label htmlFor="zoho-template-content">{t("contentLabel")}</Label><Textarea id="zoho-template-content" aria-label={t("contentLabel")} value={content} onChange={(event) => setContent(event.target.value)} rows={14} className="font-mono text-ui" /></div>
                    )}
                    {contentType === "html" ? <p className="text-2xs text-muted-foreground">{t("rawSourceHint")}</p> : null}
                  </div>
                  <DialogFooter className="border-t pt-4">
                    <Button type="button" variant="outline" onClick={() => { setEditor(null); setMutationError(null); }}>{common("cancel")}</Button>
                    <Button type="submit" disabled={pending || unknownOutcome || !name.trim() || !content.trim()}>{pending ? t("saving") : editor.mode === "create" ? t("createSubmit") : t("saveSubmit")}</Button>
                  </DialogFooter>
                </form>
              ) : importOpen && selected ? (
                <section className="flex flex-col gap-4" aria-label={t("importTitle")}>
                  <div>
                    <h2 className="text-base font-semibold">{t("importTitle")}</h2>
                    <p className="mt-1 text-sm text-muted-foreground">{t("importDescription")}</p>
                  </div>
                  {selected.content_type === "html" && selectedCompatible?.removed ? (
                    <Alert variant="warning"><TriangleAlertIcon /><AlertTitle>{t("compatibilityTitle")}</AlertTitle><AlertDescription>{t("compatibilityWarning")}</AlertDescription></Alert>
                  ) : null}
                  <p className="text-sm text-muted-foreground">{t("originalPreserved")}</p>
                  <div className="grid gap-1.5">
                    <Label htmlFor="zoho-import-subject">{t("importSubject")}</Label>
                    <Input id="zoho-import-subject" value={importSubject} onChange={(event) => setImportSubject(event.target.value)} required />
                  </div>
                  {mergeFields.length > 0 ? (
                    <fieldset className="grid gap-3 rounded-xl border p-4">
                      <legend className="px-1 text-sm font-medium">{t("mergeFieldsTitle")}</legend>
                      <p className="text-sm text-muted-foreground">{t("mergeFieldsHint")}</p>
                      {mergeFields.map((field) => {
                        const selectedMapping = mappings[field] ?? "";
                        const choice = selectedMapping.startsWith("custom:") ? "custom" : selectedMapping;
                        const customValue = selectedMapping.startsWith("custom:") ? selectedMapping.slice("custom:".length) : "";
                        return <div key={field} className="grid gap-1.5 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] sm:items-center">
                          <Label htmlFor={`zoho-map-${field}`} className="font-mono">$[{field}]$</Label>
                          <select id={`zoho-map-${field}`} aria-label={t("mappingLabel", { field })} className="h-9 rounded-lg border border-input bg-background px-3 text-sm" value={choice} onChange={(event) => setMappings((current) => ({ ...current, [field]: event.target.value }))}>
                            <option value="">{t("chooseMapping")}</option>
                            {DEFAULT_EMAIL_VARIABLES.map((preset) => <option key={preset.name} value={preset.name}>{preset.name}</option>)}
                            <option value="custom">{t("customVariable")}</option>
                            <option value="remove">{t("removeMergeField")}</option>
                          </select>
                          {choice === "custom" ? <Input className="sm:col-start-2" aria-label={t("customVariableFor", { field })} placeholder="recipient.id" value={customValue} onChange={(event) => setMappings((current) => ({ ...current, [field]: `custom:${event.target.value}` }))} /> : null}
                        </div>;
                      })}
                      {invalidCustomFields.length > 0 ? <p role="alert" className="text-sm text-destructive">{t("invalidOfficeVariable")}</p> : null}
                    </fieldset>
                  ) : null}
                  <DialogFooter className="border-t pt-4">
                    <Button type="button" variant="outline" onClick={() => setImportOpen(false)}>{common("cancel")}</Button>
                    <Button type="button" disabled={!importSubject.trim() || unresolvedFields.length > 0 || invalidCustomFields.length > 0} onClick={continueImport}>{t("continueImport")}</Button>
                  </DialogFooter>
                </section>
              ) : selectedQuery.isError ? (
                <div><p className="text-sm text-destructive">{t("detailFailed")}</p><Button className="mt-3" type="button" variant="outline" onClick={() => void selectedQuery.refetch()}>{t("retry")}</Button></div>
              ) : selectedQuery.isPending && selectedId ? (
                <p className="text-sm text-muted-foreground" aria-busy="true">{t("loadingDetail")}</p>
              ) : selected ? (
                <section className="flex flex-col gap-4">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="min-w-0"><h2 className="break-words text-base font-semibold">{selected.name}</h2><p className="mt-1 text-2xs text-muted-foreground">{selected.content_type.toUpperCase()} · {t("contentHash")}: <code>{selected.content_hash}</code></p></div>
                    <div className="flex flex-wrap gap-2">
                      <Button type="button" variant="outline" size="sm" onClick={startImport} disabled={unknownOutcome || !allowImport}>{t("import")}</Button>
                      <Button type="button" variant="outline" size="sm" onClick={() => beginEditor("create", selected)} disabled={unknownOutcome}><CopyIcon aria-hidden />{t("copy")}</Button>
                      <Button type="button" variant="outline" size="sm" onClick={() => beginEditor("edit", selected)} disabled={unknownOutcome}>{t("edit")}</Button>
                      <Button type="button" variant="destructive" size="sm" onClick={() => setDeleteOpen(true)} disabled={unknownOutcome}>{t("delete")}</Button>
                    </div>
                  </div>
                  {selected.content_type === "html" && selectedCompatible?.removed ? <Alert variant="warning"><TriangleAlertIcon /><AlertTitle>{t("nativeContentTitle")}</AlertTitle><AlertDescription>{t("nativeContentWarning")}</AlertDescription></Alert> : null}
                  <div className="overflow-hidden rounded-xl border">
                    <div className="border-b bg-muted/40 px-4 py-2 text-xs font-medium">{t("recipientPreview")}</div>
                    <p className="border-b px-4 py-2 text-2xs text-muted-foreground">{t("nativePreviewNetworkNotice")}</p>
                    <iframe title={t("previewTitle")} sandbox="" referrerPolicy="no-referrer" srcDoc={nativePreviewDocument} className="h-80 w-full bg-background" />
                  </div>
                  <details className="rounded-xl border px-4 py-3">
                    <summary className="cursor-pointer text-sm font-medium">{t("rawSource")}</summary>
                    <pre className="mt-3 max-h-72 overflow-auto rounded-lg bg-app-panel-muted p-3 font-mono text-2xs whitespace-pre-wrap">{selected.content}</pre>
                  </details>
                </section>
              ) : <p className="py-8 text-sm text-muted-foreground">{t("selectTemplate")}</p>}
            </div>
          </div>
        )}
      </DialogContent>

      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("deleteTitle")}</AlertDialogTitle>
            <AlertDialogDescription>{t("deleteWarning", { name: selected?.name ?? "" })}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={pending}>{common("cancel")}</AlertDialogCancel>
            <AlertDialogAction asChild>
              <Button type="button" variant="destructive" disabled={pending} onClick={(event) => { event.preventDefault(); void confirmDelete(); }}>{pending ? t("deleting") : t("confirmDelete")}</Button>
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Dialog>
  );
}

function selectedPreviewHtml(content: string): string {
  return cleanEmailHtml(content).html;
}
