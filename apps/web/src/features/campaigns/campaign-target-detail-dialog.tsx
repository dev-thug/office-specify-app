"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarPlusIcon, ExternalLink, PlusIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { useMemo, useRef, useState, type FormEvent } from "react";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { ActivityFeed } from "@/features/companies/activity-feed";
import { NotesPanel } from "@/features/companies/notes-panel";
import { activityGroup } from "@/features/companies/utils";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { ResearchEvidencePanel } from "@/features/research/research-evidence-panel";
import { getMe } from "@/lib/api/auth";
import { companyActivitiesPath, getCompany } from "@/lib/api/companies";
import { DraftSendingWorkflow } from "@/features/sending/draft-sending-workflow";
import { EmailCanvas, type EmailCanvasHandle } from "@/features/templates/email-canvas";
import { cleanEmailHtml, emailHtmlToText, renderSampleEmail, unsubscribeFooterHtml, unsubscribeFooterText } from "@/features/templates/email-authoring";
import { getMessageDraft, patchMessageDraft, type MessageDraftResponse } from "@/lib/api/sending";
import { apiFetch, idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { readEtag } from "@/lib/api/mutate";
import {
  EMAIL_TEMPLATES_PATH,
  type EmailTemplate,
  type TemplateVersion,
  type TemplateVersionSummary,
} from "@/lib/api/templates";
import {
  createSalesCampaignTargetDraft,
  type CreateSalesCampaignTargetDraftInput,
  type SalesCampaignSummary,
  type SalesCampaignTarget,
} from "@/lib/api/sales-campaigns";
import { useCursorList } from "@/lib/api/use-cursor-list";

export function CampaignTargetDetailDialog({
  campaign,
  target,
  queueNotice,
  onQueue,
  open,
  onOpenChange,
  onChanged,
}: {
  campaign: SalesCampaignSummary;
  target: SalesCampaignTarget;
  queueNotice: string | null;
  /** 카드를 "발송 대기"로 보낸다(필요하면 확정까지 자동 처리). */
  onQueue?: () => void | Promise<unknown>;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChanged: () => void | Promise<unknown>;
}) {
  const t = useTranslations("campaigns.personalized.target");
  const companySummaryQuery = useQuery({
    queryKey: ["companies", "detail", target.company_id],
    enabled: Boolean(target.company_id),
    queryFn: async () => (await getCompany(target.company_id!)).data,
    retry: false,
  });
  const [activeDraftId, setActiveDraftId] = useState<string | null>(
    target.current_draft?.draft_id ?? null
  );
  const [startNewDraft, setStartNewDraft] = useState(false);
  const [copyPending, setCopyPending] = useState(false);
  const [copySucceeded, setCopySucceeded] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);
  const copyPendingRef = useRef(false);
  const copyOperationKeyRef = useRef<string | null>(null);
  const outboundStatus = target.outbound?.status;
  const meQuery = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });
  const isHumanReviewer = meQuery.data?.kind === "human";
  const acceptedFollowUpAvailable = Boolean(
    target.stage === "accepted" &&
      target.current_draft?.status === "frozen" &&
      outboundStatus === "accepted" &&
      target.outbound?.provider_accepted === true
  );
  const canCopyReviewedDraft = Boolean(
    target.stage === "review" &&
      target.current_draft?.status === "frozen" &&
      outboundStatus === "draft" &&
      !copySucceeded
  );
  const canCreateAcceptedFollowUp = Boolean(
    acceptedFollowUpAvailable && isHumanReviewer && !copySucceeded
  );
  const company = target.company_name ?? "";

  function reloadRemoteDraft() {
    setActiveDraftId(target.current_draft?.draft_id ?? null);
    setStartNewDraft(false);
  }

  async function copyReviewedDraft(explicitFollowUp = false) {
    const sourceDraftId = target.current_draft?.draft_id;
    const allowed = explicitFollowUp ? canCreateAcceptedFollowUp : canCopyReviewedDraft;
    if (!sourceDraftId || !allowed || copyPendingRef.current) return;
    copyPendingRef.current = true;
    copyOperationKeyRef.current ??= idempotencyKey();
    setCopyPending(true);
    setCopyError(null);
    try {
      const source = await getMessageDraft(sourceDraftId);
      const input: CreateSalesCampaignTargetDraftInput = {
        subject: source.data.subject,
        body_text: source.data.body_text,
        body_html: source.data.body_html ?? null,
        template_version_id: source.data.template_version_id ?? null,
        variables: source.data.variables ?? {},
      };
      if (explicitFollowUp) input.explicit_follow_up = true;
      const created = await createSalesCampaignTargetDraft(
        campaign.id,
        target.id,
        input,
        { idempotencyKey: copyOperationKeyRef.current }
      );
      copyOperationKeyRef.current = null;
      setCopySucceeded(true);
      setActiveDraftId(created.data.draft_id);
      setStartNewDraft(false);
      try {
        await onChanged();
      } catch {
        setCopyError(t("copyRefreshError"));
      }
    } catch (cause) {
      if (
        cause instanceof ApiError &&
        cause.problem.retryable !== true &&
        cause.status < 500
      ) {
        copyOperationKeyRef.current = null;
      }
      setCopyError(t("copyReviewedDraftError"));
    } finally {
      copyPendingRef.current = false;
      setCopyPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92vh] overflow-y-auto sm:max-w-[min(96vw,84rem)]">
        <DialogHeader>
          <DialogTitle>{company || target.contact_name || target.email_address}</DialogTitle>
          <DialogDescription>
            {target.contact_name ? `${target.contact_name} · ` : ""}
            {target.email_address}
          </DialogDescription>
        </DialogHeader>

        <section aria-label={t("sendingIdentity")} className="flex flex-wrap items-start justify-between gap-2 rounded-xl border border-border bg-app-panel-muted px-3 py-3 text-sm">
          <div className="grid gap-1">
            <p>{t("from")}: {campaign.from_name} &lt;{campaign.from_address}&gt;</p>
            <p className="text-muted-foreground">{t("replyTo")}: {campaign.reply_to ?? campaign.from_address}</p>
          </div>
          <Link
            href="/app/settings/mail-accounts"
            className="text-xs text-primary underline underline-offset-2"
          >
            {t("changeSender")}
          </Link>
        </section>

        {queueNotice ? (
          <Alert variant="warning">
            <AlertDescription>{queueNotice}</AlertDescription>
          </Alert>
        ) : null}

        {target.current_draft && (outboundStatus === "failed" || outboundStatus === "dead" || outboundStatus === "canceled") && !startNewDraft ? (
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-app-panel-muted px-3 py-3">
            <p className="text-sm text-muted-foreground">{t("replacementDraftAllowed")}</p>
            <Button
              type="button"
              size="sm"
              onClick={() => {
                setActiveDraftId(null);
                setStartNewDraft(true);
              }}
            >
              {t("newDraft")}
            </Button>
          </div>
        ) : null}
        {canCopyReviewedDraft ? (
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-app-panel-muted px-3 py-3">
            <p className="text-sm text-muted-foreground">{t("frozenReviewCopyNote")}</p>
            <Button type="button" size="sm" disabled={copyPending} onClick={() => void copyReviewedDraft()}>
              {copyPending ? t("copyingReviewedDraft") : t("copyReviewedDraft")}
            </Button>
            {copyError ? <p role="alert" className="w-full text-sm text-destructive">{copyError}</p> : null}
          </div>
        ) : null}
        {outboundStatus === "unknown" ? (
          <Alert variant="warning">
            <AlertDescription>{t("unknownDraftLocked")}</AlertDescription>
          </Alert>
        ) : null}
        {outboundStatus === "queued" || outboundStatus === "claimed" || outboundStatus === "submitting" ? (
          <Alert variant="warning">
            <AlertDescription>{t("queuedDraftLocked")}</AlertDescription>
          </Alert>
        ) : null}
        {acceptedFollowUpAvailable ? (
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-app-panel-muted px-3 py-3">
            <p className="text-sm text-muted-foreground">{t("acceptedFollowUpUnavailable")}</p>
            {isHumanReviewer ? (
              <Button
                type="button"
                size="sm"
                disabled={copyPending || !canCreateAcceptedFollowUp}
                onClick={() => void copyReviewedDraft(true)}
              >
                {copyPending ? t("writingFollowUpDraft") : t("writeFollowUpDraft")}
              </Button>
            ) : meQuery.isPending ? (
              <p role="status" className="text-xs text-muted-foreground">{t("checkingFollowUpPermission")}</p>
            ) : (
              <p role="status" className="text-xs text-muted-foreground">{t("acceptedFollowUpHumanOnly")}</p>
            )}
            {copyError ? <p role="alert" className="w-full text-sm text-destructive">{copyError}</p> : null}
          </div>
        ) : null}
        <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)]">
          <div className="min-w-0">
        <CampaignTargetDraftEditor
              campaign={campaign}
              target={target}
              draftId={!startNewDraft ? activeDraftId ?? undefined : undefined}
              onCreated={(draft) => {
                setActiveDraftId(draft.draft_id);
                setStartNewDraft(false);
              }}
              onReloadRemote={reloadRemoteDraft}
              onChanged={onChanged}
              onQueue={onQueue}
            />
          </div>
        {target.company_id ? (
          <aside aria-label={t("researchReference")} className="flex min-w-0 flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div>
                <h2 className="text-sm font-semibold">{t("researchReference")}</h2>
                <p className="mt-1 text-xs text-muted-foreground">{t("sources")}</p>
              </div>
              <Button asChild variant="outline" size="xs">
                <Link href={`/app/companies/${encodeURIComponent(target.company_id)}`}>
                  <ExternalLink aria-hidden />
                  {t("openCompany")}
                </Link>
              </Button>
            </div>
            <section className="grid gap-2 rounded-xl border border-border bg-app-panel-muted px-3 py-3">
              <h3 className="text-sm font-semibold">{t("companySummary")}</h3>
              {companySummaryQuery.isPending ? (
                <Skeleton className="h-16 w-full rounded-xl" />
              ) : companySummaryQuery.isError ? (
                <Alert variant="warning">
                  <AlertDescription>{t("companySummaryLoadError")}</AlertDescription>
                </Alert>
              ) : companySummaryQuery.data ? (
                <>
                  {companySummaryQuery.data.description ? (
                    <p className="text-sm">{companySummaryQuery.data.description}</p>
                  ) : (
                    <p className="text-sm text-muted-foreground">{t("companySummaryEmpty")}</p>
                  )}
                  <dl className="grid gap-x-4 gap-y-2 text-sm sm:grid-cols-2">
                    {companySummaryQuery.data.industry ? (
                      <div>
                        <dt className="text-xs text-muted-foreground">{t("industry")}</dt>
                        <dd>{companySummaryQuery.data.industry}</dd>
                      </div>
                    ) : null}
                    {companySummaryQuery.data.domain ? (
                      <div>
                        <dt className="text-xs text-muted-foreground">{t("domain")}</dt>
                        <dd>{companySummaryQuery.data.domain}</dd>
                      </div>
                    ) : null}
                  </dl>
                </>
              ) : null}
            </section>
            <ResearchEvidencePanel companyId={target.company_id} initialReviewStatus="all" />
            <section className="flex flex-col gap-2 rounded-xl border border-border px-3 py-3">
              <h3 className="text-sm font-semibold">{t("crmNotes")}</h3>
              <NotesPanel
                ns="companies"
                companyId={target.company_id}
                canWrite={false}
                feed={
                  <ActivityFeed
                    path={companyActivitiesPath(target.company_id)}
                    ns="companies"
                    filter={(activity) => activityGroup(activity) === "note"}
                    emptyTitle={t("crmNotesEmpty")}
                    emptyDescription={t("crmNotesEmptyDescription")}
                    queryKeySuffix={`campaign-card-${target.id}-notes`}
                    includeNoteBody
                  />
                }
              />
            </section>
          </aside>
        ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function CampaignTargetDraftEditor({
  campaign,
  target,
  draftId,
  onCreated,
  onReloadRemote,
  onChanged,
  onQueue,
}: {
  campaign: SalesCampaignSummary;
  target: SalesCampaignTarget;
  onQueue?: () => void | Promise<unknown>;
  draftId?: string;
  onCreated: (draft: MessageDraftResponse) => void;
  onReloadRemote: () => void;
  onChanged: () => void | Promise<unknown>;
}) {
  const t = useTranslations("campaigns.personalized.target");
  const ta = useTranslations("templates.authoring");
  const queryClient = useQueryClient();
  const etagRef = useRef<string | undefined>(undefined);
  const operationKeyRef = useRef<string | null>(null);
  type DraftEditorValues = {
    subject: string;
    bodyText: string;
    bodyHtml: string;
    variablesJson: string;
  };
  const [editorOverrides, setEditorOverrides] = useState<{
    key: string;
    values: Partial<DraftEditorValues>;
  } | null>(null);
  const [saveConflict, setSaveConflict] = useState(false);
  const [selectedTemplateId, setSelectedTemplateId] = useState("");
  const [selectedVersionId, setSelectedVersionId] = useState("");
  const [bodyEditorModeOverride, setBodyEditorModeOverride] = useState<"plain" | "rich" | null>(null);
  const [pending, setPending] = useState(false);
  const [queueing, setQueueing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<Partial<Record<"subject" | "body" | "variables" | "personal", string>>>({});
  const subjectRef = useRef<HTMLInputElement>(null);
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  const canvasRef = useRef<EmailCanvasHandle>(null);
  const insertTarget = useRef<"subject" | "body">("body");

  const draftQueryKey = ["sales-campaigns", "draft", campaign.id, target.id, draftId] as const;
  const draftQuery = useQuery({
    queryKey: draftQueryKey,
    enabled: Boolean(draftId),
    queryFn: async () => {
      const result = await getMessageDraft(draftId!);
      etagRef.current = readEtag(result) ?? undefined;
      return result.data;
    },
    retry: false,
  });
  const draft = draftQuery.data;
  const canEdit = !draftId || (draft?.status === "draft" && !draft.outbound_message_id);


  const templatesQuery = useCursorList<EmailTemplate>({
    queryKey: ["templates", "campaign-target-list"],
    path: EMAIL_TEMPLATES_PATH,
    limit: 50,
    enabled: !draftId,
  });
  const templates =
    templatesQuery.data?.pages.flatMap((page) => page.data) ?? [];
  const activeTemplates = templates.filter((template) => !template.archived_at);
  const versionsQuery = useCursorList<TemplateVersionSummary>({
    queryKey: ["templates", "campaign-target-versions", selectedTemplateId],
    path: `${EMAIL_TEMPLATES_PATH}/${encodeURIComponent(selectedTemplateId)}/versions`,
    limit: 100,
    enabled: !draftId && Boolean(selectedTemplateId),
  });
  const versions = versionsQuery.data?.pages.flatMap((page) => page.data) ?? [];
  const templateVersionQuery = useQuery({
    queryKey: ["templates", "campaign-target-version", selectedVersionId],
    enabled: !draftId && Boolean(selectedVersionId),
    queryFn: async () =>
      (
        await apiFetch<TemplateVersion>(
          `/api/v1/template-versions/${encodeURIComponent(selectedVersionId)}`
        )
      ).data,
    retry: false,
  });
  const templateVersion = templateVersionQuery.data;
  const editorKey = draftId
    ? `draft:${draftId}`
    : `target:${target.id}:template:${selectedVersionId || "none"}`;
  const baseEditorValues: DraftEditorValues = draft
    ? {
        subject: draft.subject,
        bodyText: draft.body_text,
        bodyHtml: draft.body_html ?? "",
        variablesJson: JSON.stringify(draft.variables ?? {}, null, 2),
      }
    : templateVersion && templateVersion.template_version_id === selectedVersionId
      ? {
          subject: templateVersion.subject,
          bodyText: templateVersion.body_text,
          bodyHtml: templateVersion.body_html ?? "",
          variablesJson: "{}",
        }
      : { subject: "", bodyText: "", bodyHtml: "", variablesJson: "{}" };
  const editorValues = {
    ...baseEditorValues,
    ...(editorOverrides?.key === editorKey ? editorOverrides.values : {}),
  };
  const { subject, bodyText, bodyHtml, variablesJson } = editorValues;
  const bodyEditorMode = bodyEditorModeOverride ?? (bodyHtml.trim() ? "rich" : "plain");
  // 리치 편집만 쓰는 경우 별도 일반 텍스트 없이도 저장할 수 있도록 HTML에서 만든다.
  const effectiveBodyText = bodyText.trim()
    ? bodyText
    : bodyHtml.trim()
      ? emailHtmlToText(bodyHtml)
      : "";
  const editorFields = Object.keys(baseEditorValues) as (keyof DraftEditorValues)[];
  const isDirty = editorFields.some(
    (field) => editorValues[field] !== baseEditorValues[field]
  );
  const remoteConflict = Boolean(
    draftId &&
      draft &&
      (!target.current_draft ||
        target.current_draft.draft_id !== draft.draft_id ||
        target.current_draft.version > draft.version)
  ) || saveConflict;
  function updateEditorValue<K extends keyof DraftEditorValues>(
    field: K,
    value: DraftEditorValues[K]
  ) {
    operationKeyRef.current = null;
    setError(null);
    setFieldErrors({});
    setEditorOverrides((current) => ({
      key: editorKey,
      values: {
        ...(current?.key === editorKey ? current.values : {}),
        [field]: value,
      },
    }));
  }

  const parsedVariables = useMemo(() => {
    try {
      const value: unknown = JSON.parse(variablesJson);
      if (!value || typeof value !== "object" || Array.isArray(value)) return null;
      return value as Record<string, unknown>;
    } catch {
      return null;
    }
  }, [variablesJson]);
  // 템플릿에 {{message.body}} 가 있으면 JSON 대신 전용 입력란으로 회사별 본문을 쓴다.
  const personalBodyDeclared =
    Boolean(templateVersion?.variable_schema?.properties?.["message.body"]) ||
    (parsedVariables !== null && "message.body" in parsedVariables);
  const personalBody =
    typeof parsedVariables?.["message.body"] === "string"
      ? (parsedVariables["message.body"] as string)
      : "";
  // 발송 시 서버가 CRM에서 채우는 값 — 미리보기에도 같은 값을 보여 준다.
  const recipientValues = useMemo(() => {
    const values: Record<string, string> = {};
    if (target.company_name) values["company.name"] = target.company_name;
    if (target.contact_name) values["contact.name"] = target.contact_name;
    return values;
  }, [target.company_name, target.contact_name]);
  const sampleValues = useMemo(
    () => ({
      ...recipientValues,
      ...Object.fromEntries(
        Object.entries(parsedVariables ?? {}).map(([key, value]) => [key, String(value ?? "")])
      ),
    }),
    [parsedVariables, recipientValues]
  );
  function insertVariable(name: string) {
    // 수신 거부는 링크만 덩그러니 두지 않고 안내 문장째 넣는다.
    if (name === "compliance.unsubscribe_url" && insertTarget.current === "body") {
      if (bodyEditorMode === "rich") {
        canvasRef.current?.insertHtml(unsubscribeFooterHtml({ before: ta("unsubscribeFooterBefore"), link: ta("unsubscribeFooterLink"), after: ta("unsubscribeFooterAfter") }));
        return;
      }
      insertPlain(`\n\n${unsubscribeFooterText(ta("unsubscribeFooterText"))}`);
      return;
    }
    insertPlain(`{{${name}}}`, name);
  }
  function insertPlain(token: string, name?: string) {
    if (insertTarget.current === "subject" || bodyEditorMode === "plain") {
      const field = insertTarget.current === "subject" ? "subject" : "bodyText";
      const element = field === "subject" ? subjectRef.current : bodyRef.current;
      const current = field === "subject" ? subject : bodyText;
      const start = element?.selectionStart ?? current.length;
      const end = element?.selectionEnd ?? start;
      updateEditorValue(field, current.slice(0, start) + token + current.slice(end));
      requestAnimationFrame(() => {
        element?.focus();
        element?.setSelectionRange(start + token.length, start + token.length);
      });
    } else if (name) {
      canvasRef.current?.insertVariable(name);
    }
  }
  const sampleHtml = useMemo(
    () => renderSampleEmail(bodyHtml, sampleValues),
    [bodyHtml, sampleValues]
  );
  const sampleSubject = useMemo(
    () => renderSampleEmail(subject, sampleValues),
    [subject, sampleValues]
  );

  async function reloadRemote() {
    setEditorOverrides(null);
    setSaveConflict(false);
    setError(null);
    if (target.current_draft?.draft_id !== draftId) {
      onReloadRemote();
      return;
    }
    await draftQuery.refetch();
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending || remoteConflict) return;
    // 저장 버튼은 항상 눌러 볼 수 있고, 비어 있는 항목은 여기서 알려 준다.
    const problems: Partial<Record<"subject" | "body" | "variables" | "personal", string>> = {};
    if (!subject.trim()) problems.subject = t("subjectRequired");
    if (!effectiveBodyText.trim()) problems.body = t("bodyRequired");
    if (parsedVariables === null) problems.variables = t("variablesInvalid");
    else if (personalBodyDeclared && !personalBody.trim()) problems.personal = t("personalBodyRequired");
    setFieldErrors(problems);
    const missing = Object.keys(problems);
    if (missing.length > 0) {
      const labels: Record<string, string> = {
        subject: t("subject"),
        body: t("fieldBody"),
        variables: t("variables"),
        personal: t("personalBodyLabel"),
      };
      setError(`${t("fillRequired")} ${missing.map((key) => labels[key]).join(", ")}`);
      const focusId =
        problems.subject
          ? "campaign-target-subject"
          : problems.personal
            ? "campaign-target-personal-body"
            : problems.body
            ? bodyEditorMode === "plain"
              ? "campaign-target-body"
              : "campaign-target-body-rich"
            : "campaign-target-variables";
      requestAnimationFrame(() => document.getElementById(focusId)?.focus());
      return;
    }
    if (parsedVariables === null) return;
    operationKeyRef.current ??= idempotencyKey();
    setPending(true);
    setError(null);
    try {
      if (draftId && draft) {
        const result = await patchMessageDraft(
          draftId,
          {
            subject: subject.trim(),
            body_text: effectiveBodyText,
            body_html: bodyHtml.trim() ? bodyHtml : null,
            variables: draft.template_version_id ? parsedVariables : undefined,
          },
          {
            etag: etagRef.current ?? draft.version,
            idempotencyKey: operationKeyRef.current,
          }
        );
        etagRef.current = readEtag(result) ?? undefined;
        queryClient.setQueryData(draftQueryKey, result.data);
        setEditorOverrides(null);
        setSaveConflict(false);
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: draftQueryKey }),
          queryClient.invalidateQueries({ queryKey: ["sending", "draft", draftId] }),
        ]);
      } else if (!draftId) {
        const result = await createSalesCampaignTargetDraft(
          campaign.id,
          target.id,
          {
            subject: subject.trim(),
            body_text: effectiveBodyText,
            body_html: bodyHtml.trim() ? bodyHtml : null,
            template_version_id: selectedVersionId || null,
            variables: parsedVariables,
          },
          { idempotencyKey: operationKeyRef.current }
        );
        etagRef.current = readEtag(result) ?? undefined;
        await onChanged();
        onCreated(result.data);
      }
      operationKeyRef.current = null;
      if (draftId) await onChanged();
    } catch (cause) {
      if (
        cause instanceof ApiError &&
        cause.problem.retryable !== true &&
        cause.status < 500
      ) {
        operationKeyRef.current = null;
      }
      if (
        cause instanceof ApiError &&
        (cause.status === 409 || cause.status === 412 ||
          ["VERSION_MISMATCH", "PRECONDITION_REQUIRED"].includes(cause.code))
      ) {
        setSaveConflict(true);
        void onChanged();
      }
      setError(t("draftError"));
    } finally {
      setPending(false);
    }
  }

  const outboundId =
    draft?.outbound_message_id ?? target.outbound?.outbound_message_id ?? null;
  const outboundQuery = useQuery({
    queryKey: ["sending", "outbound", outboundId],
    enabled: Boolean(outboundId),
    queryFn: async () =>
      (await apiFetch<import("@/lib/api/sending").OutboundMessageResponse>(
        `/api/v1/outbound-messages/${encodeURIComponent(outboundId!)}`
      )).data,
    retry: false,
  });
  const outbound = outboundQuery.data;

  return (
    <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
      <div>
        <h2 className="text-sm font-semibold">
          {t("draftTitle", { company: target.company_name ?? target.contact_name ?? "" })}
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">{t("draftDescription")}</p>
      </div>

      {remoteConflict ? (
        <Alert variant="warning">
          <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
            <span>{t("remoteConflict")}</span>
            <Button type="button" variant="outline" size="xs" onClick={() => void reloadRemote()}>
              {t("reloadRemote")}
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      {draftId && draftQuery.isPending ? (
        <Skeleton className="h-32 rounded-2xl" />
      ) : draftId && draftQuery.isError ? (
        <Alert variant="destructive">
          <AlertDescription>{t("draftError")}</AlertDescription>
        </Alert>
      ) : canEdit ? (
        <form className="flex flex-col gap-4" noValidate onSubmit={(event) => void save(event)}>
          {!draftId ? (
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="grid gap-1.5">
                <Label htmlFor="campaign-target-template">{t("template")}</Label>
                <select
                  id="campaign-target-template"
                  className="h-9 rounded-md border border-input bg-background px-3 text-sm"
                  value={selectedTemplateId || "none"}
                  disabled={templatesQuery.isPending || pending}
                  onChange={(event) => {
                    const id = event.target.value === "none" ? "" : event.target.value;
                    setSelectedTemplateId(id);
                    setSelectedVersionId("");
                    setBodyEditorModeOverride(null);
                  }}
                >
                  <option value="none">{t("noTemplate")}</option>
                  {activeTemplates.map((template) => (
                    <option key={template.template_id} value={template.template_id}>
                      {template.name}
                    </option>
                  ))}
                </select>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="campaign-target-template-version">{t("version")}</Label>
                <select
                  id="campaign-target-template-version"
                  className="h-9 rounded-md border border-input bg-background px-3 text-sm"
                  value={selectedVersionId || "none"}
                  disabled={!selectedTemplateId || versionsQuery.isPending || pending}
                  onChange={(event) => {
                    setSelectedVersionId(event.target.value === "none" ? "" : event.target.value);
                    setBodyEditorModeOverride(null);
                  }}
                >
                  <option value="none">{t("noTemplate")}</option>
                  {versions.map((version) => (
                    <option key={version.template_version_id} value={version.template_version_id}>
                      {t("versionNumber", { no: version.version_no })}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          ) : draft?.template_version_id ? (
            <p className="text-2xs text-muted-foreground">
              {t("templateAttached")}
            </p>
          ) : null}
          {templateVersionQuery.isError ? (
            <Alert variant="destructive">
              <AlertDescription>{t("templateLoadError")}</AlertDescription>
            </Alert>
          ) : null}
          {personalBodyDeclared ? (
            <div className="grid gap-1.5 rounded-xl border border-primary/30 bg-primary/5 p-3">
              <Label htmlFor="campaign-target-personal-body">{t("personalBodyLabel")}</Label>
              <Textarea
                id="campaign-target-personal-body"
                value={personalBody}
                rows={6}
                maxLength={5000}
                placeholder={t("personalBodyPlaceholder")}
                aria-invalid={fieldErrors.personal ? true : undefined}
                onChange={(event) =>
                  updateEditorValue(
                    "variablesJson",
                    JSON.stringify(
                      { ...(parsedVariables ?? {}), "message.body": event.target.value },
                      null,
                      2
                    )
                  )
                }
              />
              {fieldErrors.personal ? (
                <p role="alert" className="text-2xs text-destructive">{fieldErrors.personal}</p>
              ) : null}
              <p className="text-2xs text-muted-foreground">{t("personalBodyHint")}</p>
            </div>
          ) : null}
          <VariableChips
            t={t}
            values={recipientValues}
            bookingAvailable={Boolean(campaign.meeting_type_id)}
            richMode={bodyEditorMode === "rich"}
            onInsert={insertVariable}
            onInsertBookingButton={() => canvasRef.current?.insertBookingCta(t("variableBookingButtonLabel"))}
          />
          <div className="grid gap-1.5">
            <Label htmlFor="campaign-target-subject">{t("subject")}</Label>
            <Input
              ref={subjectRef}
              onFocus={() => { insertTarget.current = "subject"; }}
              id="campaign-target-subject"
              value={subject}
              onChange={(event) => {
                updateEditorValue("subject", event.target.value);
              }}
              maxLength={998}
              aria-invalid={fieldErrors.subject ? true : undefined}
              placeholder={t("subjectPlaceholder")}
            />
            {fieldErrors.subject ? (
              <p role="alert" className="text-2xs text-destructive">{fieldErrors.subject}</p>
            ) : null}
          </div>
          <div className="grid gap-2">
            <div role="group" aria-label={t("messageFormat")} className="flex flex-wrap gap-2">
              <Button
                type="button"
                size="sm"
                variant={bodyEditorMode === "plain" ? "secondary" : "outline"}
                aria-pressed={bodyEditorMode === "plain"}
                onClick={() => setBodyEditorModeOverride("plain")}
              >
                {t("plainTextMode")}
              </Button>
              <Button
                type="button"
                size="sm"
                variant={bodyEditorMode === "rich" ? "secondary" : "outline"}
                aria-pressed={bodyEditorMode === "rich"}
                onClick={() => setBodyEditorModeOverride("rich")}
              >
                {t("richEmailMode")}
              </Button>
            </div>
            <p className="text-2xs text-muted-foreground">{t("bodyModeHint")}</p>
            {bodyEditorMode === "plain" ? (
              <div className="grid gap-1.5">
                <Label htmlFor="campaign-target-body">{t("plainTextFallback")}</Label>
                <Textarea
                  ref={bodyRef}
                  onFocus={() => { insertTarget.current = "body"; }}
                  id="campaign-target-body"
                  value={bodyText}
                  onChange={(event) => updateEditorValue("bodyText", event.target.value)}
                  rows={8}
                  aria-invalid={fieldErrors.body ? true : undefined}
                />
                {fieldErrors.body ? (
                  <p role="alert" className="text-2xs text-destructive">{fieldErrors.body}</p>
                ) : null}
                <p className="text-2xs text-muted-foreground">{t("plainTextFallbackHint")}</p>
              </div>
            ) : (
              <div className="grid gap-1.5">
                <Label id="campaign-target-body-rich" tabIndex={-1}>{t("bodyHtml")}</Label>
                <EmailCanvas
                  ref={canvasRef}
                  html={bodyHtml}
                  previewHtml={sampleHtml}
                  previewSubject={sampleSubject}
                  previewButtonLabel={t("samplePreview")}
                  onChange={(value) => updateEditorValue("bodyHtml", value)}
                  onFocus={() => { insertTarget.current = "body"; }}
                />
                {fieldErrors.body ? (
                  <p role="alert" className="text-2xs text-destructive">{fieldErrors.body}</p>
                ) : null}
                <p className="text-2xs text-muted-foreground">{t("previewNotice")}</p>
              </div>
            )}
          </div>
          <details
            className="rounded-xl border border-border px-3 py-2"
            open={variablesJson.trim() !== "{}" || parsedVariables === null}
          >
            <summary className="cursor-pointer text-sm font-medium">{t("advancedVariables")}</summary>
            <div className="mt-3">
          <div className="grid gap-1.5">
                <Label htmlFor="campaign-target-variables">{t("variables")}</Label>
                <Textarea
                  id="campaign-target-variables"
                  value={variablesJson}
                  onChange={(event) => {
                    updateEditorValue("variablesJson", event.target.value);
                  }}
                  rows={4}
                  spellCheck={false}
                  aria-invalid={parsedVariables === null}
                />
                <p className="text-2xs text-muted-foreground">{t("variablesHint")}</p>
                {parsedVariables === null ? (
                  <p role="alert" className="text-2xs text-destructive">{t("variablesInvalid")}</p>
                ) : null}
              </div>
            </div>
          </details>
          {error ? (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          ) : null}
          <div className="flex flex-wrap items-center justify-end gap-2">
            {draftId && onQueue && !isDirty && (!target.outbound || target.outbound.status === "draft") ? (
              <Button
                type="button"
                variant="outline"
                disabled={pending || remoteConflict || queueing}
                onClick={() => {
                  setQueueing(true);
                  void Promise.resolve(onQueue()).finally(() => setQueueing(false));
                }}
              >
                {queueing ? t("queueing") : t("sendQueue")}
              </Button>
            ) : null}
            <Button type="submit" disabled={pending || remoteConflict}>
              {pending ? t("savingDraft") : draftId ? t("saveDraft") : t("createDraft")}
            </Button>
          </div>
        </form>
      ) : draft ? (
        <div className="rounded-lg border border-border bg-app-panel-muted px-3 py-2 text-sm">
          <p className="font-medium">{draft.subject}</p>
          <p className="mt-1 whitespace-pre-wrap text-muted-foreground">{draft.body_text}</p>
          <p className="mt-2 text-2xs text-muted-foreground">{t("frozenReadOnly")}</p>
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">{t("noDraft")}</p>
      )}

      {draftId ? (
        <DraftSendingWorkflow
          draftId={draftId}
          mailAccountId={campaign.mail_account_id}
          allowSubmit={false}
          salesCampaignId={campaign.id}
          lockedDeliveryChannelId={campaign.delivery_channel_id}
          campaignMeetingTypeId={campaign.meeting_type_id ?? undefined}
          freezeBlocked={isDirty || remoteConflict}
          onChanged={async () => {
            await Promise.all([
              queryClient.invalidateQueries({ queryKey: draftQueryKey }),
              onChanged(),
            ]);
          }}
        />
      ) : null}

      {outboundId ? (
        <section aria-label={t("recipientPreview")} className="flex flex-col gap-2 rounded-2xl border border-border px-4 py-3">
          <h3 className="text-sm font-medium">{t("recipientPreview")}</h3>
          {outboundQuery.isPending ? (
            <Skeleton className="h-40 w-full rounded-2xl" />
          ) : outboundQuery.isError ? (
            <Alert variant="warning"><AlertDescription>{t("snapshotLoadError")}</AlertDescription></Alert>
          ) : outbound?.body_html ? (
            <iframe
              title={t("recipientPreview")}
              sandbox=""
              referrerPolicy="no-referrer"
              srcDoc={emailPreviewDocument(outbound.body_html)}
              className="h-80 w-full rounded-xl border border-border bg-background"
            />
          ) : outbound ? (
            <div>
              <p className="font-medium">{outbound.subject}</p>
              <pre className="mt-2 whitespace-pre-wrap font-sans text-sm">{outbound.body_text}</pre>
            </div>
          ) : null}
        </section>
      ) : null}
    </section>
  );
}

function emailPreviewDocument(html: string) {
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{margin:0;padding:16px;font:14px/1.55 system-ui,sans-serif;overflow-wrap:anywhere}img{max-width:100%;height:auto}</style></head><body>${cleanEmailHtml(html).html}</body></html>`;
}

const DRAFT_VARIABLES = [
  { name: "company.name", label: "variableCompany" },
  { name: "contact.name", label: "variableContact" },
  { name: "sender.display_name", label: "variableSender" },
  { name: "meeting.booking_url", label: "variableBooking" },
  { name: "compliance.unsubscribe_url", label: "variableUnsubscribe" },
] as const;

/** 클릭 한 번으로 마지막에 커서가 있던 제목/본문에 {{변수}}를 넣는다. */
function VariableChips({
  t,
  values,
  bookingAvailable,
  richMode,
  onInsert,
  onInsertBookingButton,
}: {
  t: ReturnType<typeof useTranslations>;
  values: Record<string, string>;
  bookingAvailable: boolean;
  richMode: boolean;
  onInsert: (name: string) => void;
  onInsertBookingButton: () => void;
}) {
  const chips = DRAFT_VARIABLES.filter((variable) => variable.name !== "meeting.booking_url" || bookingAvailable);
  return (
    <div className="grid gap-2 rounded-xl bg-muted/50 p-3">
      <div>
        <p className="text-sm font-medium">{t("variablesInsert")}</p>
        <p className="text-2xs text-muted-foreground">{t("variablesInsertHint")}</p>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {chips.map((variable) => (
          <Button
            key={variable.name}
            type="button"
            size="xs"
            variant="outline"
            title={`{{${variable.name}}}`}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onInsert(variable.name)}
          >
            <PlusIcon aria-hidden />
            {t(variable.label)}
            {values[variable.name] ? (
              <span className="max-w-32 truncate text-muted-foreground">· {values[variable.name]}</span>
            ) : null}
          </Button>
        ))}
        {bookingAvailable && richMode ? (
          <Button
            type="button"
            size="xs"
            variant="outline"
            onMouseDown={(event) => event.preventDefault()}
            onClick={onInsertBookingButton}
          >
            <CalendarPlusIcon aria-hidden />
            {t("variableBookingButton")}
          </Button>
        ) : null}
      </div>
      {!bookingAvailable ? <p className="text-2xs text-muted-foreground">{t("variableBookingUnavailable")}</p> : null}
    </div>
  );
}
