"use client";

import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import {
  useCallback,
  useMemo,
  useState,
} from "react";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeftIcon,
  CheckIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { toast } from "sonner";

import { ConfirmIrreversible } from "@/components/patterns/confirm-irreversible";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { getMe } from "@/lib/api/auth";
import { apiFetch } from "@/lib/api/client";
import {
  createCampaign,
  createLaunch,
  createRevision,
  DELIVERY_CHANNELS_PATH,
  getCampaign,
  getCampaignLaunch,
  normalizeLaunch,
  requestLaunchApproval,
  runPreflight,
  type CampaignStepInput,
  type CreateCampaignInput,
  type DeliveryChannel,
  type PreflightResult,
} from "@/lib/api/campaigns";
import {
  EMAIL_TEMPLATES_PATH,
  type EmailTemplate,
  type TemplateVersion,
  type TemplateVersionSummary,
} from "@/lib/api/templates";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { useMailAccounts } from "@/lib/api/settings";
import { RenderPreview } from "@/features/templates";
import { cn } from "@/lib/utils";

import { useApiErrorMessage } from "./api-error-message";
import { AudiencePicker, type AudienceSelection } from "./audience-picker";
import { formatDateTime, shortHash } from "./format";
import {
  channelGateReasons,
  bookingUrlBindingMissing,
  latestRevision,
  passedRecipientIds,
  summarizePreflight,
  withMeetingTypeBinding,
} from "./rules";
import { MEETING_TYPES_LIST_LIMIT, MEETING_TYPES_PATH, MEETING_TYPES_QUERY_KEY, PRINCIPALS_PATH, principalDisplayName, type MeetingType, type PrincipalRef } from "@/lib/api/meetings";
import { useIdempotencyKey } from "./use-idempotency-key";

export type BuilderMode = "create" | "revision" | "clone";

const STEP_COUNT = 6;

/** ISO 요일(1=월…7=일) → 메시지 키 */
const WEEKDAY_KEYS = [
  "weekdayMon",
  "weekdayTue",
  "weekdayWed",
  "weekdayThu",
  "weekdayFri",
  "weekdaySat",
  "weekdaySun",
] as const;

interface StepDraft {
  /** 로컬 행 키 — 서버 필드가 아니다. */
  key: string;
  templateId: string;
  versionId: string;
}

let stepSeq = 0;
const newStep = (): StepDraft => ({
  key: `s${++stepSeq}`,
  templateId: "",
  versionId: "",
});

/** IANA 시간대 유효성 — Intl 이 모르는 값이면 거절한다. */
function isValidTimezone(value: string): boolean {
  if (!value.trim()) return false;
  try {
    new Intl.DateTimeFormat("en", { timeZone: value.trim() });
    return true;
  } catch {
    return false;
  }
}

/** datetime-local 값을 RFC 3339 UTC로 변환한다. */
function localToIso(value: string): string | undefined {
  if (!value) return undefined;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? undefined : d.toISOString();
}

export function CampaignBuilder({
  mode,
  sourceCampaignId,
}: {
  mode: BuilderMode;
  sourceCampaignId?: string;
}) {
  const t = useTranslations("campaigns.builder");
  const tList = useTranslations("campaigns.list");
  const tDetail = useTranslations("campaigns.detail");
  const tAudience = useTranslations("campaigns.audience");
  const tPreflight = useTranslations("campaigns.preflight");
  const tGate = useTranslations("campaigns.channelGate");
  const tReason = useTranslations("campaigns.reasonCodes");
  const tCommon = useTranslations("common");
  const locale = useLocale();
  const qc = useQueryClient();
  const errorMessage = useApiErrorMessage("campaigns");

  const [step, setStep] = useState(1);
  const [errors, setErrors] = useState<string[]>([]);

  // ── 단계 1: 목표·채널 ──
  const [name, setName] = useState("");
  const [objective, setObjective] = useState("");
  const [mailAccountId, setMailAccountId] = useState("");
  const [channelId, setChannelId] = useState("");
  // ── 단계 2: 대상 ──
  const [audience, setAudience] = useState<AudienceSelection>(new Map());
  // ── 단계 3: 시퀀스 ──
  const [steps, setSteps] = useState<StepDraft[]>([newStep()]);
  const [meetingTypeId, setMeetingTypeId] = useState("");
  const [stopOnReply, setStopOnReply] = useState(true);
  const [stopOnBooking, setStopOnBooking] = useState(true);
  const [previewStepKey, setPreviewStepKey] = useState<string | null>(null);
  // ── 단계 4: 시간·한도 ──
  const [timezone, setTimezone] = useState("Asia/Seoul");
  const [weekdays, setWeekdays] = useState<number[]>([1, 2, 3, 4, 5]);
  const [windowStart, setWindowStart] = useState("09:00");
  const [windowEnd, setWindowEnd] = useState("17:00");
  const [dailyCap, setDailyCap] = useState("");
  const [minGap, setMinGap] = useState("");
  const [notBefore, setNotBefore] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  // ── 단계 5/6: 생성·동결·승인 ──
  const [created, setCreated] = useState<{
    campaignId: string;
    revisionId: string;
  } | null>(null);
  const [createdBlocking, setCreatedBlocking] = useState<string[]>([]);
  const [preflight, setPreflight] = useState<PreflightResult | null>(null);
  const [launchId, setLaunchId] = useState<string | null>(null);
  const [freezeOpen, setFreezeOpen] = useState(false);
  const [approvalReason, setApprovalReason] = useState("");
  const [approvalDone, setApprovalDone] = useState(false);
  const [pending, setPending] = useState<"draft" | "preflight" | "freeze" | "approve" | null>(null);

  const createIdem = useIdempotencyKey();
  const freezeIdem = useIdempotencyKey();
  const approveIdem = useIdempotencyKey();

  // revision/clone 모드 — 원본 캠페인을 불러와 폼을 시드한다.
  const needsSource = mode !== "create" && Boolean(sourceCampaignId);
  const source = useQuery({
    queryKey: ["campaigns", "detail", sourceCampaignId],
    enabled: needsSource,
    queryFn: async () => (await getCampaign(sourceCampaignId!)).data,
    retry: false,
  });
  // revision/clone 시드 — 원본 도착 시 렌더 단계에서 한 번만 폼을 채운다
  // (React 권장 "adjusting state when props change" 패턴).
  const [seededFrom, setSeededFrom] = useState<unknown>(null);
  const src = source.data;
  const sourceRevision = src?.revision ?? latestRevision(src?.revisions);
  const sourceSteps = sourceRevision?.steps;
  const sourceHasLegacyWorkflow = Boolean(
    needsSource &&
      sourceRevision &&
      (!sourceSteps ||
        sourceSteps.length !== 1 ||
        sourceSteps[0]?.step_order !== 1 ||
        sourceSteps[0]?.delay_hours !== 0)
  );
  if (needsSource && src && seededFrom !== src) {
    setSeededFrom(src);
    const rev = sourceRevision;
    setName(mode === "clone" ? `${src.name} ${t("cloneSuffix")}` : src.name);
    setObjective(src.objective ?? "");
    if (rev) {
      setMailAccountId(rev.mail_account_id);
      setChannelId(rev.delivery_channel_id);
      setTimezone(rev.timezone);
      setStopOnReply(rev.stop_on_reply);
      setStopOnBooking(rev.stop_on_booking);
      const rules = rev.schedule_rules ?? {};
      setMeetingTypeId(
        typeof rules.meeting_type_id === "string" ? rules.meeting_type_id : ""
      );
      if (Array.isArray(rules.weekdays)) setWeekdays(rules.weekdays);
      if (rules.window_start) setWindowStart(String(rules.window_start).slice(0, 5));
      if (rules.window_end) setWindowEnd(String(rules.window_end).slice(0, 5));
      if (rules.internal_daily_cap != null) setDailyCap(String(rules.internal_daily_cap));
      if (rules.minimum_gap_seconds != null) setMinGap(String(rules.minimum_gap_seconds));
      // Do not silently truncate a legacy sequence into a different campaign.
      if (!sourceHasLegacyWorkflow && Array.isArray(rev.steps) && rev.steps.length === 1) {
        setSteps(
          rev.steps.map((s) => ({
            key: `s${++stepSeq}`,
            templateId: "",
            versionId: s.template_version_id,
          }))
        );
      }
    }
  }

  const me = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });
  const mailAccountList = useMailAccounts();
  const mailAccounts = me.data?.mail_accounts ?? [];
  const mailAccountStatuses = (mailAccountList.data?.pages ?? []).flatMap((p) => p.data ?? []);
  const selectedMailAccountStatus = mailAccountStatuses.find((account) => account.id === mailAccountId)?.status;

  const channels = useCursorList<DeliveryChannel>({
    queryKey: ["campaigns", "delivery-channels"],
    path: DELIVERY_CHANNELS_PATH,
    limit: 200,
  });
  const channelRows = useMemo(
    () =>
      (channels.data?.pages.flatMap((p) => p.data) ?? []).filter(
        (channel) =>
          channel.purpose === "marketing" &&
          channel.capabilities?.transport === "smtp" &&
          (!mailAccountId || channel.mail_account_id === mailAccountId)
      ),
    [channels.data, mailAccountId]
  );
  const selectedChannel = channelRows.find((c) => c.id === channelId) ?? null;
  const gateReasons = channelGateReasons(selectedChannel);
  const gateBlocked = gateReasons.length > 0 || createdBlocking.length > 0;

  const meetingTypeList = useCursorList<MeetingType>({
    queryKey: MEETING_TYPES_QUERY_KEY,
    path: MEETING_TYPES_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
  });
  const meetingTypePrincipals = useCursorList<PrincipalRef>({
    queryKey: ["meetings", "principals"],
    path: PRINCIPALS_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
  });
  const meetingTypeRows = useMemo(
    () =>
      (meetingTypeList.data?.pages.flatMap((p) => p.data) ?? []).filter(
        (meetingType) => meetingType.status === "active"
      ),
    [meetingTypeList.data]
  );
  const meetingTypeHostNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const person of meetingTypePrincipals.data?.pages.flatMap((page) => page.data) ?? []) {
      const name = principalDisplayName(person);
      if (name) names.set(person.id, name);
    }
    return names;
  }, [meetingTypePrincipals.data]);

  // 템플릿·버전 선택 — 버전은 템플릿 선택 시 lazy 조회한다.
  const templates = useCursorList<EmailTemplate>({
    queryKey: ["templates", "list"],
    path: EMAIL_TEMPLATES_PATH,
    limit: 200,
  });
  const templateRows = useMemo(
    () =>
      (templates.data?.pages.flatMap((p) => p.data) ?? []).filter(
        (tpl) => !tpl.archived_at
      ),
    [templates.data]
  );
  const [versionsByTemplate, setVersionsByTemplate] = useState<
    Record<string, TemplateVersionSummary[]>
  >({});
  // 버전 목록은 템플릿 선택 시점(사용자 액션)에 lazy 조회한다.
  // 시드된 단계의 versionId 는 템플릿을 다시 고르기 전까지 ID 텍스트로 둔다
  // (revision.steps 에는 template_id 가 없다).
  const loadVersions = useCallback(
    async (templateId: string) => {
      if (!templateId || versionsByTemplate[templateId]) return;
      try {
        const res = await apiFetch<TemplateVersionSummary[]>(
          `${EMAIL_TEMPLATES_PATH}/${encodeURIComponent(templateId)}/versions?limit=200`
        );
        setVersionsByTemplate((prev) => ({ ...prev, [templateId]: res.data }));
      } catch {
        // 버전 목록 로드 실패는 선택 불가로 충분히 표현된다 — 재시도는 템플릿 재선택으로.
      }
    },
    [versionsByTemplate]
  );

  const stepVersionQueries = useQueries({
    queries: steps.map((step) => ({
      queryKey: ["templates", "version", step.versionId],
      enabled: Boolean(step.versionId),
      queryFn: async () => (await apiFetch<TemplateVersion>(`/api/v1/template-versions/${encodeURIComponent(step.versionId)}`)).data,
    })),
  });
  const selectedStepVersions = stepVersionQueries.map((query) => query.data);
  const previewIndex = steps.findIndex((step) => step.key === previewStepKey);
  const selectedVersionForPreview = selectedStepVersions[previewIndex] ?? null;
  const bookingUrlRequired = bookingUrlBindingMissing(selectedStepVersions, "");
  const bookingUrlBindingIsMissing = bookingUrlBindingMissing(
    selectedStepVersions,
    meetingTypeId,
    meetingTypeList.isSuccess ? meetingTypeRows.map((meetingType) => meetingType.id) : undefined
  );
  const selectedMeetingType = meetingTypeRows.find(
    (meetingType) => meetingType.id === meetingTypeId
  );

  const locked = created !== null; // 초안 생성 후 구성 변경은 새 revision으로만

  function buildInput(): CreateCampaignInput {
    const scheduleRules = {
      weekdays,
      window_start: windowStart,
      window_end: windowEnd,
      ...(dailyCap ? { internal_daily_cap: Number(dailyCap) } : {}),
      ...(minGap ? { minimum_gap_seconds: Number(minGap) } : {}),
      ...(localToIso(notBefore)
        ? { launch_not_before: localToIso(notBefore) }
        : {}),
      ...(localToIso(expiresAt)
        ? { launch_expires_at: localToIso(expiresAt) }
        : {}),
    };
    return {
      name: name.trim(),
      objective: objective.trim() || undefined,
      mail_account_id: mailAccountId,
      delivery_channel_id: channelId,
      timezone: timezone.trim(),
      schedule_rules: withMeetingTypeBinding(scheduleRules, meetingTypeId),
      steps: [
        {
          step_order: 1,
          template_version_id: steps[0]?.versionId ?? "",
          delay_hours: 0,
        } satisfies CampaignStepInput,
      ],
      stop_on_reply: stopOnReply,
      stop_on_booking: stopOnBooking,
    };
  }

  function validateStep(n: number): boolean {
    const errs: string[] = [];
    if (n === 1) {
      if (!name.trim()) errs.push(t("validationName"));
      if (!mailAccountId) errs.push(t("validationMailbox"));
      if (!channelId || !selectedChannel) errs.push(t("validationChannel"));
    } else if (n === 2) {
      if (audience.size === 0) errs.push(t("validationAudience"));
    } else if (n === 3) {
      if (steps.length !== 1 || !steps[0]?.versionId)
        errs.push(t("validationSteps"));
      if (bookingUrlBindingIsMissing)
        errs.push(t("validationMeetingTypeRequired"));
    } else if (n === 4) {
      if (!isValidTimezone(timezone)) errs.push(t("validationTimezone"));
      if (weekdays.length === 0) errs.push(t("validationWeekdays"));
      if (windowStart && windowEnd && windowEnd <= windowStart)
        errs.push(t("validationWindow"));
    }
    setErrors(errs);
    return errs.length === 0;
  }

  function goNext() {
    if (!validateStep(step)) return;
    setStep((s) => Math.min(STEP_COUNT, s + 1));
    setErrors([]);
  }
  function goBack() {
    setStep((s) => Math.max(1, s - 1));
    setErrors([]);
  }

  async function submitDraft() {
    if (!validateStep(4)) return;
    setPending("draft");
    setErrors([]);
    const input = buildInput();
    try {
      if (mode === "revision" && sourceCampaignId) {
        const revisionInput = {
          delivery_channel_id: input.delivery_channel_id,
          timezone: input.timezone,
          schedule_rules: input.schedule_rules,
          steps: input.steps,
          stop_on_reply: input.stop_on_reply,
          stop_on_booking: input.stop_on_booking,
        };
        const res = await createRevision(sourceCampaignId, revisionInput, {
          idempotencyKey: createIdem.key(),
        });
        createIdem.settle();
        setCreated({
          campaignId: sourceCampaignId,
          revisionId: res.data.revision_id ?? "",
        });
        setCreatedBlocking([]);
      } else {
        const res = await createCampaign(input, {
          idempotencyKey: createIdem.key(),
        });
        createIdem.settle();
        setCreated({
          campaignId: res.data.campaign_id,
          revisionId: res.data.revision_id,
        });
        // 채널 미허용이면 초안까지만 — global block을 저장해 게이트에 쓴다.
        setCreatedBlocking(res.data.blocking_reasons ?? []);
      }
      toast.success(t("draftCreated"));
    } catch (err) {
      createIdem.settle(err);
      setErrors([errorMessage(err)]);
    } finally {
      setPending(null);
    }
  }

  async function runPreflightNow() {
    if (!created) return;
    setPreflight(null);
    setLaunchId(null);
    setApprovalDone(false);
    setPending("preflight");
    setErrors([]);
    try {
      const res = await runPreflight(created.campaignId, {
        revision_id: created.revisionId,
        email_address_ids: [...audience.keys()],
      });
      setPreflight(res.data);
      setCreatedBlocking(res.data.global_blocking_reasons ?? createdBlocking);
    } catch (err) {
      setErrors([errorMessage(err)]);
    } finally {
      setPending(null);
    }
  }

  async function freeze() {
    if (!created) return;
    const passed = passedRecipientIds(preflight);
    if (!preflight || passed.length === 0) {
      setErrors([tDetail("noEligibleRecipients")]);
      setFreezeOpen(false);
      return;
    }
    setPending("freeze");
    try {
      const res = await createLaunch(
        created.campaignId,
        {
          revision_id: created.revisionId,
          // 검증을 통과한 주소만 동결 대상에 포함한다.
          email_address_ids: passed,
        },
        { idempotencyKey: freezeIdem.key() }
      );
      freezeIdem.settle();
      setLaunchId(res.data.launch_id ?? res.data.id ?? null);
      setFreezeOpen(false);
      setStep(6);
    } catch (err) {
      freezeIdem.settle(err);
      setErrors([errorMessage(err)]);
      setFreezeOpen(false);
    } finally {
      setPending(null);
    }
  }

  // 동결 job 폴링 — building 동안만 재조회한다.
  const launch = useQuery({
    queryKey: ["campaigns", "launch", launchId],
    enabled: Boolean(launchId),
    refetchInterval: (q) =>
      q.state.data?.status === "building" ? 2000 : false,
    queryFn: async () => normalizeLaunch((await getCampaignLaunch(launchId!)).data),
  });
  const launchFrozen =
    launch.data && launch.data.status !== "building" && launch.data.manifest_hash;

  async function submitApproval() {
    if (!launchId || !launch.data?.manifest_hash) return;
    setPending("approve");
    setErrors([]);
    try {
      await requestLaunchApproval(
        launchId,
        {
          expected_manifest_hash: launch.data.manifest_hash,
          reason: approvalReason.trim() || undefined,
        },
        { idempotencyKey: approveIdem.key() }
      );
      approveIdem.settle();
      setApprovalDone(true);
      qc.invalidateQueries({ queryKey: ["campaigns"] });
      qc.invalidateQueries({ queryKey: ["approvals"] });
      toast.success(tDetail("approvalRequested"));
    } catch (err) {
      approveIdem.settle(err);
      setErrors([errorMessage(err)]);
    } finally {
      setPending(null);
    }
  }

  const stepNames = [
    t("step1"),
    t("step2"),
    t("step3"),
    t("step4"),
    t("step5"),
    t("step6"),
  ];

  const title =
    mode === "revision"
      ? t("revisionTitle")
      : mode === "clone"
        ? t("cloneTitle")
        : t("title");

  if (needsSource && source.isLoading) {
    return (
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-4 px-6 py-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-64 w-full rounded-2xl" />
      </div>
    );
  }
  if (needsSource && (source.isError || (source.isSuccess && !source.data))) {
    return (
      <div className="mx-auto w-full max-w-4xl px-6 py-6">
        {source.isError ? (
          <ErrorState error={source.error} onRetry={() => source.refetch()} />
        ) : (
          <Alert variant="warning">
            <AlertDescription>{t("sourceMissing")}</AlertDescription>
          </Alert>
        )}
      </div>
    );
  }
  if (sourceHasLegacyWorkflow && source.data && sourceRevision) {
    return (
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-5 px-6 py-6">
        <Link
          href={mode === "revision" ? `/app/campaigns/${sourceCampaignId}` : "/app/campaigns"}
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeftIcon className="size-4" aria-hidden />
          {tList("title")}
        </Link>
        <PageHeader
          title={mode === "revision" ? t("revisionFor", { name: source.data.name }) : title}
          description={t("description")}
        />
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{t("legacyWorkflowTitle")}</AlertTitle>
          <AlertDescription>
            {t("legacyWorkflowDescription", { count: sourceSteps?.length ?? 0 })}
          </AlertDescription>
        </Alert>
        <Link
          href="/app/campaigns/new"
          className="w-fit text-sm font-medium underline underline-offset-4"
        >
          {t("startNewCampaign")}
        </Link>
      </div>
    );
  }

  const mailboxAddress =
    mailAccounts.find((m) => m.id === mailAccountId)?.address ?? mailAccountId;

  return (
    <div className="mx-auto flex w-full max-w-4xl flex-col gap-5 px-6 py-6">
      <div>
        <Link
          href={
            mode === "revision" && sourceCampaignId
              ? `/app/campaigns/${sourceCampaignId}`
              : "/app/campaigns"
          }
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeftIcon className="size-4" aria-hidden />
          {tList("title")}
        </Link>
      </div>

      <PageHeader
        title={
          mode === "revision" && source.data
            ? t("revisionFor", { name: source.data.name })
            : title
        }
        description={t("description")}
      />

      {/* 6단계 진행 표시 (§7.5) */}
      <ol
        aria-label={t("stepLabel", {
          current: step,
          total: STEP_COUNT,
          name: stepNames[step - 1] ?? "",
        })}
        className="grid gap-2 sm:grid-cols-3 lg:grid-cols-6"
      >
        {stepNames.map((n, i) => {
          const idx = i + 1;
          const done = idx < step;
          const current = idx === step;
          return (
            <li
              key={n}
              aria-current={current ? "step" : undefined}
              className={cn(
                "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm",
                current
                  ? "border-ring bg-accent/50 font-medium"
                  : "border-border text-muted-foreground"
              )}
            >
              <span
                className={cn(
                  "flex size-5 shrink-0 items-center justify-center rounded-full text-2xs",
                  done || current
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted text-muted-foreground"
                )}
              >
                {done ? <CheckIcon className="size-3" aria-hidden /> : idx}
              </span>
              <span className="truncate">{n}</span>
            </li>
          );
        })}
      </ol>
      <p className="text-2xs text-muted-foreground" role="status">
        {t("stepLabel", {
          current: step,
          total: STEP_COUNT,
          name: stepNames[step - 1] ?? "",
        })}
      </p>

      {locked ? (
        <Alert variant="info">
          <AlertDescription>{t("lockedNote")}</AlertDescription>
        </Alert>
      ) : null}

      {selectedMailAccountStatus === "reauth_required" ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertDescription>{t("mailboxReauthRequired")}</AlertDescription>
        </Alert>
      ) : null}

      {errors.length > 0 ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertDescription>
            <ul className="list-disc pl-4">
              {errors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      ) : null}

      {/* ── 1. 목표·채널 ─────────────────────────────── */}
      {step === 1 ? (
        <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
          {mode !== "revision" ? (
            <>
              <div className="grid gap-1.5">
                <Label htmlFor="cb-name">{t("name")}</Label>
                <Input
                  id="cb-name"
                  required
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder={t("namePlaceholder")}
                  disabled={locked}
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="cb-objective">{t("objective")}</Label>
                <Textarea
                  id="cb-objective"
                  rows={2}
                  value={objective}
                  onChange={(e) => setObjective(e.target.value)}
                  placeholder={t("objectivePlaceholder")}
                  disabled={locked}
                />
              </div>
            </>
          ) : null}

          {sourceCampaignId && channelId && !selectedChannel ? (
            <Alert variant="warning">
              <TriangleAlertIcon />
              <AlertDescription>{t("retiredChannelUnavailable")}</AlertDescription>
            </Alert>
          ) : null}

          <div className="grid gap-1.5">
            <Label htmlFor="cb-mailbox">{t("mailAccount")}</Label>
            <Select
              value={mailAccountId}
              onValueChange={(value) => {
                setMailAccountId(value);
                setChannelId("");
              }}
              disabled={locked}
            >
              <SelectTrigger id="cb-mailbox" aria-label={t("mailAccount")}>
                <SelectValue placeholder={t("selectMailAccount")} />
              </SelectTrigger>
              <SelectContent>
                {mailAccounts.map((m) => (
                  <SelectItem key={m.id} value={m.id}>
                    {m.address}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="cb-channel">{t("deliveryChannel")}</Label>
            <Select
              value={channelId}
              onValueChange={setChannelId}
              disabled={locked || !mailAccountId}
            >
              <SelectTrigger id="cb-channel" aria-label={t("deliveryChannel")}>
                <SelectValue placeholder={t("selectChannel")} />
              </SelectTrigger>
              <SelectContent>
                {channelRows.map((c) => (
                  <SelectItem key={c.id} value={c.id}>
                    <span className="flex items-center gap-2">
                      {c.name}
                      <StatusBadge status={c.policy_status} />
                    </span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {mailAccountId && channelRows.length === 0 && !channels.isLoading ? (
              <p className="text-2xs text-muted-foreground">{t("noChannels")}</p>
            ) : null}
            {selectedChannel ? (
              <div className="flex flex-wrap items-center gap-2 text-2xs text-muted-foreground">
                <span>
                  {t("channelPurpose")}: {selectedChannel.purpose}
                </span>
                <span className="inline-flex items-center gap-1">
                  {t("channelPolicy")}:
                  <StatusBadge status={selectedChannel.policy_status} />
                </span>
                {!selectedChannel.enabled ? (
                  <Badge variant="secondary">{t("channelDisabled")}</Badge>
                ) : null}
              </div>
            ) : null}
          </div>

          {gateReasons.length > 0 ? (
            <Alert variant="warning">
              <TriangleAlertIcon />
              <AlertDescription>{t("channelBlockedHint")}</AlertDescription>
            </Alert>
          ) : null}
        </section>
      ) : null}

      {/* ── 2. 대상 ──────────────────────────────────── */}
      {step === 2 ? (
        <AudiencePicker
          selected={audience}
          onChange={(next) => {
            setAudience(next);
            setPreflight(null);
            setCreatedBlocking([]);
            setLaunchId(null);
            setApprovalDone(false);
          }}
        />
      ) : null}

      {/* ── 3. 단일 메시지 ──────────────────────────── */}
      {step === 3 ? (
        <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
          <div>
            <h3 className="text-sm font-medium">{t("singleMessage")}</h3>
            <p className="mt-0.5 text-2xs text-muted-foreground">
              {t("immediateMessageHint")}
            </p>
          </div>
          {steps.map((s) => (
            <div
              key={s.key}
              className="flex flex-col gap-3 rounded-lg border border-border px-3 py-3"
            >
              <span className="text-sm font-medium">{t("message")}</span>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="grid gap-1.5">
                  <Label>{t("template")}</Label>
                  <Select
                    value={s.templateId}
                    onValueChange={(v) => {
                      setSteps((ss) =>
                        ss.map((x) =>
                          x.key === s.key
                            ? { ...x, templateId: v, versionId: "" }
                            : x
                        )
                      );
                      void loadVersions(v);
                    }}
                    disabled={locked}
                  >
                    <SelectTrigger aria-label={t("template")}>
                      <SelectValue placeholder={t("selectTemplate")} />
                    </SelectTrigger>
                    <SelectContent>
                      {templateRows.map((tpl) => (
                        <SelectItem key={tpl.template_id} value={tpl.template_id}>
                          {tpl.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
                <div className="grid gap-1.5">
                  <Label>{t("version")}</Label>
                  <Select
                    value={s.versionId}
                    onValueChange={(v) =>
                      setSteps((ss) =>
                        ss.map((x) =>
                          x.key === s.key ? { ...x, versionId: v } : x
                        )
                      )
                    }
                    disabled={locked || !s.templateId}
                  >
                    <SelectTrigger aria-label={t("version")}>
                      <SelectValue placeholder={t("selectVersion")} />
                    </SelectTrigger>
                    <SelectContent>
                      {(versionsByTemplate[s.templateId] ?? []).map((v) => (
                        <SelectItem key={v.template_version_id} value={v.template_version_id}>
                          v{v.version_no}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {s.versionId && !(versionsByTemplate[s.templateId] ?? []).some((v) => v.template_version_id === s.versionId) ? (
                    <p className="text-2xs text-muted-foreground">{t("savedVersionUnavailable")}</p>
                  ) : null}
                </div>
              </div>
              {s.versionId ? (
                <div>
                  <Button
                    type="button"
                    variant="outline"
                    size="xs"
                    onClick={() =>
                      setPreviewStepKey((k) => (k === s.key ? null : s.key))
                    }
                  >
                    {t("personalizationPreview")}
                  </Button>
                </div>
              ) : null}
            </div>
          ))}
          <div className="grid gap-1.5">
            <Label htmlFor="cb-meeting-type">
              {t("meetingType")}
              {bookingUrlRequired ? ` (${t("required")})` : ""}
            </Label>
            <Select
              value={meetingTypeId || "none"}
              onValueChange={(value) =>
                setMeetingTypeId(value === "none" ? "" : value)
              }
              disabled={locked || meetingTypeList.isLoading || meetingTypeList.isError}
            >
              <SelectTrigger
                id="cb-meeting-type"
                aria-label={t("meetingType")}
                aria-required={bookingUrlRequired}
              >
                <SelectValue placeholder={t("selectMeetingType")} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">{t("noMeetingType")}</SelectItem>
                {meetingTypeId && !selectedMeetingType ? (
                  <SelectItem value={meetingTypeId} disabled>
                    {meetingTypeList.isLoading ? t("loadingSavedMeetingType") : t("savedMeetingTypeUnavailable")}
                  </SelectItem>
                ) : null}
                {meetingTypeRows.map((meetingType) => (
                  <SelectItem key={meetingType.id} value={meetingType.id}>
                    {meetingType.name} · {t("meetingTypeDuration", { count: meetingType.duration_minutes })}
                    {meetingTypeHostNames.get(meetingType.host_principal_id) ? ` · ${t("meetingTypeHost", { name: meetingTypeHostNames.get(meetingType.host_principal_id)! })}` : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-2xs text-muted-foreground">
              {bookingUrlRequired
                ? t("meetingTypeRequiredHint")
                : t("meetingTypeHint")}
            </p>
            {meetingTypeList.isLoading ? (
              <p className="text-2xs text-muted-foreground" role="status">
                {t("loadingMeetingTypes")}
              </p>
            ) : null}
            {meetingTypeList.isError ? (
              <div className="flex items-center gap-2 text-2xs text-destructive" role="alert">
                <p>{t("meetingTypesLoadFailed")}</p>
                <Button
                  type="button"
                  variant="link"
                  size="xs"
                  className="h-auto p-0 text-destructive"
                  onClick={() => void meetingTypeList.refetch()}
                >
                  {tCommon("retry")}
                </Button>
              </div>
            ) : null}
            {meetingTypeList.isSuccess && meetingTypeRows.length === 0 && !meetingTypeId ? (
              <p className="text-2xs text-muted-foreground">
                {t("noActiveMeetingTypes")}{" "}
                <Link href="/app/meeting-types" className="underline underline-offset-4">
                  {t("manageMeetingTypes")}
                </Link>
              </p>
            ) : null}
            {meetingTypeList.hasNextPage ? (
              <Button type="button" variant="link" size="xs" className="h-auto w-fit p-0" disabled={meetingTypeList.isFetchingNextPage} onClick={() => void meetingTypeList.fetchNextPage()}>
                {meetingTypeList.isFetchingNextPage ? t("loadingMoreMeetingTypes") : t("loadMoreMeetingTypes")}
              </Button>
            ) : null}
          </div>
          <fieldset className="grid gap-2">
            <legend className="text-sm font-medium">{t("stopConditions")}</legend>
            <label className="flex items-center gap-2 text-sm">
              <Switch
                checked={stopOnReply}
                onCheckedChange={(v) => setStopOnReply(v === true)}
                disabled={locked}
              />
              {t("stopOnReply")}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <Switch
                checked={stopOnBooking}
                onCheckedChange={(v) => setStopOnBooking(v === true)}
                disabled={locked}
              />
              {t("stopOnBooking")}
            </label>
          </fieldset>

          {previewStepKey && selectedVersionForPreview ? (
            <RenderPreview version={selectedVersionForPreview} />
          ) : null}
        </section>
      ) : null}

      {/* ── 4. 시간·한도 ─────────────────────────────── */}
      {step === 4 ? (
        <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
          <div className="grid gap-1.5">
            <Label htmlFor="cb-tz">{t("timezone")}</Label>
            <Input
              id="cb-tz"
              value={timezone}
              onChange={(e) => setTimezone(e.target.value)}
              placeholder={t("timezonePlaceholder")}
              className="max-w-xs font-mono text-ui"
              disabled={locked}
            />
          </div>

          <fieldset className="grid gap-2">
            <legend className="text-sm font-medium">{t("weekdays")}</legend>
            <div className="flex flex-wrap gap-1.5">
              {[1, 2, 3, 4, 5, 6, 7].map((d) => {
                const key = WEEKDAY_KEYS[d - 1] ?? "weekdayMon";
                const on = weekdays.includes(d);
                return (
                  <Button
                    key={d}
                    type="button"
                    size="xs"
                    variant={on ? "default" : "outline"}
                    aria-pressed={on}
                    disabled={locked}
                    onClick={() =>
                      setWeekdays((w) =>
                        on ? w.filter((x) => x !== d) : [...w, d].sort()
                      )
                    }
                    className="w-10 rounded-full"
                  >
                    {t(key)}
                  </Button>
                );
              })}
            </div>
          </fieldset>

          <div className="grid gap-3 sm:grid-cols-2">
            <div className="grid gap-1.5">
              <Label htmlFor="cb-wstart">{t("windowStart")}</Label>
              <Input
                id="cb-wstart"
                type="time"
                value={windowStart}
                onChange={(e) => setWindowStart(e.target.value)}
                disabled={locked}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="cb-wend">{t("windowEnd")}</Label>
              <Input
                id="cb-wend"
                type="time"
                value={windowEnd}
                onChange={(e) => setWindowEnd(e.target.value)}
                disabled={locked}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="cb-cap">{t("dailyCap")}</Label>
              <Input
                id="cb-cap"
                inputMode="numeric"
                value={dailyCap}
                onChange={(e) => setDailyCap(e.target.value)}
                disabled={locked}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="cb-gap">{t("minGap")}</Label>
              <Input
                id="cb-gap"
                inputMode="numeric"
                value={minGap}
                onChange={(e) => setMinGap(e.target.value)}
                disabled={locked}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="cb-notbefore">{t("notBefore")}</Label>
              <Input
                id="cb-notbefore"
                type="datetime-local"
                value={notBefore}
                onChange={(e) => setNotBefore(e.target.value)}
                disabled={locked}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="cb-expires">{t("expiresAt")}</Label>
              <Input
                id="cb-expires"
                type="datetime-local"
                value={expiresAt}
                onChange={(e) => setExpiresAt(e.target.value)}
                disabled={locked}
              />
            </div>
          </div>
          <p className="text-2xs text-muted-foreground">{t("internalCapNote")}</p>
        </section>
      ) : null}

      {/* ── 5. 검토·동결 ─────────────────────────────── */}
      {step === 5 ? (
        <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
          <h3 className="text-sm font-medium">{t("reviewSummary")}</h3>
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">{t("name")}</dt>
            <dd className="font-medium">{name}</dd>
            <dt className="text-muted-foreground">{t("mailAccount")}</dt>
            <dd>{mailboxAddress || "—"}</dd>
            <dt className="text-muted-foreground">{t("deliveryChannel")}</dt>
            <dd className="flex items-center gap-2">
              {selectedChannel?.name ?? "—"}
              {selectedChannel ? (
                <StatusBadge status={selectedChannel.policy_status} />
              ) : null}
            </dd>
            <dt className="text-muted-foreground">{t("step2")}</dt>
            <dd>{tAudience("selectedCount", { count: audience.size })}</dd>
            <dt className="text-muted-foreground">{t("step3")}</dt>
            <dd>{t("immediateSend")}</dd>
            <dt className="text-muted-foreground">{t("meetingType")}</dt>
            <dd>
              {selectedMeetingType?.name ??
                (meetingTypeId
                  ? t("savedMeetingTypeUnavailable")
                  : t("noMeetingType"))}
            </dd>
            <dt className="text-muted-foreground">{t("step4")}</dt>
            <dd>
              {timezone} ·{" "}
              {tDetail("scheduleWindow", {
                start: windowStart,
                end: windowEnd,
              })}
            </dd>
          </dl>

          {/* 채널 게이트 — 차단되면 동결·승인 요청 불가, 초안만 가능 (§7.5-1) */}
          {gateBlocked ? (
            <Alert variant="warning">
              <TriangleAlertIcon />
              <AlertTitle>{tGate("title")}</AlertTitle>
              <AlertDescription>
                {[...new Set([...gateReasons, ...createdBlocking])].map((r) => (
                  <Badge key={r} variant="amber-subtle" className="mr-1.5">
                    {tReason.has(r) ? tReason(r) : r}
                  </Badge>
                ))}
              </AlertDescription>
            </Alert>
          ) : null}

          {!created ? (
            <div>
              <Button
                type="button"
                size="sm"
                disabled={pending !== null}
                onClick={submitDraft}
              >
                {pending === "draft"
                  ? t("creatingDraft")
                  : mode === "revision"
                    ? t("createRevisionSubmit")
                    : t("createDraft")}
              </Button>
            </div>
          ) : (
            <div className="flex flex-col gap-4">
              <Alert variant="success">
                <CheckIcon />
                <AlertDescription>{t("draftCreated")}</AlertDescription>
              </Alert>

              {/* preflight — 비구속 검증 (B.3.7) */}
              <div className="flex flex-col gap-3 rounded-lg border border-border px-3 py-3">
                <div className="flex items-center justify-between gap-2">
                  <div>
                    <h4 className="text-sm font-medium">
                      {tPreflight("title")}
                    </h4>
                    <p className="text-2xs text-muted-foreground">
                      {tPreflight("description")}
                    </p>
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={pending !== null}
                    onClick={runPreflightNow}
                  >
                    {pending === "preflight"
                      ? tPreflight("running")
                      : tPreflight("run")}
                  </Button>
                </div>
                {preflight ? (
                  <PreflightSummary result={preflight} locale={locale} />
                ) : null}
              </div>

              <div>
                <Button
                  type="button"
                  size="sm"
                  disabled={
                    pending !== null ||
                    gateBlocked ||
                    !preflight ||
                    passedRecipientIds(preflight).length === 0
                  }
                  onClick={() => setFreezeOpen(true)}
                >
                  {tDetail("freeze")}
                </Button>
                {!preflight ? (
                  <p className="mt-1 text-2xs text-muted-foreground">
                    {tDetail("preflightFirst")}
                  </p>
                ) : null}
                {preflight && passedRecipientIds(preflight).length === 0 ? (
                  <p className="mt-1 text-2xs text-muted-foreground" role="status">
                    {tDetail("noEligibleRecipients")}
                  </p>
                ) : null}
              </div>
            </div>
          )}
        </section>
      ) : null}

      {/* ── 6. 승인·실행 ─────────────────────────────── */}
      {step === 6 ? (
        <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
          {launch.isLoading || launch.data?.status === "building" ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Skeleton className="size-4 rounded-full" />
              {tDetail("buildingLaunch")}
            </div>
          ) : launch.isError ? (
            <ErrorState error={launch.error} onRetry={() => launch.refetch()} />
          ) : launchFrozen ? (
            <>
              <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
                <dt className="text-muted-foreground">
                  {tDetail("manifestHash")}
                </dt>
                <dd
                  className="font-mono text-ui break-all"
                  title={launch.data.manifest_hash ?? undefined}
                >
                  {shortHash(launch.data.manifest_hash)}
                </dd>
                <dt className="text-muted-foreground">
                  {tDetail("colRecipients")}
                </dt>
                <dd>
                  {tAudience("selectedCount", {
                    count:
                      launch.data.recipients_count ??
                      passedRecipientIds(preflight).length,
                  })}
                </dd>
                <dt className="text-muted-foreground">
                  {tDetail("launchExpires")}
                </dt>
                <dd>{formatDateTime(launch.data.expires_at, locale)}</dd>
              </dl>

              {approvalDone ? (
                <div className="flex flex-col gap-3">
                  <Alert variant="success">
                    <CheckIcon />
                    <AlertDescription>
                      {tDetail("approvalPending")}
                    </AlertDescription>
                  </Alert>
                  <div className="flex gap-2">
                    <Button asChild size="sm">
                      <Link href={`/app/campaigns/${created?.campaignId}`}>
                        {t("goToDetail")}
                      </Link>
                    </Button>
                    <Button asChild size="sm" variant="outline">
                      <Link href="/app/approvals">
                        {tDetail("goToApprovals")}
                      </Link>
                    </Button>
                  </div>
                </div>
              ) : (
                <>
                  <div className="grid gap-1.5">
                    <Label htmlFor="cb-approval-reason">
                      {tDetail("approvalReason")}
                    </Label>
                    <Textarea
                      id="cb-approval-reason"
                      rows={3}
                      value={approvalReason}
                      onChange={(e) => setApprovalReason(e.target.value)}
                      placeholder={tDetail("approvalReasonPlaceholder")}
                    />
                  </div>
                  <p className="text-2xs text-muted-foreground">
                    {tDetail("approvalRequestDescription")}
                  </p>
                  <div>
                    <Button
                      type="button"
                      size="sm"
                      disabled={pending !== null || gateBlocked}
                      onClick={submitApproval}
                    >
                      {pending === "approve"
                        ? tCommon("loading")
                        : tDetail("requestApproval")}
                    </Button>
                  </div>
                </>
              )}
            </>
          ) : (
            <Alert variant="warning">
              <TriangleAlertIcon />
              <AlertDescription>
                <StatusBadge status={launch.data?.status ?? "unknown"} />
              </AlertDescription>
            </Alert>
          )}
        </section>
      ) : null}

      {/* 하단 탐색 */}
      <div className="flex items-center justify-between">
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={step === 1}
          onClick={goBack}
        >
          {t("back")}
        </Button>
        {step < 5 ? (
          <Button
            type="button"
            size="sm"
            disabled={step === 1 && Boolean(sourceCampaignId && channelId && !selectedChannel)}
            onClick={goNext}
          >
            {t("next")}
          </Button>
        ) : null}
      </div>

      <ConfirmIrreversible
        open={freezeOpen}
        onOpenChange={setFreezeOpen}
        title={tDetail("freezeTitle")}
        description={tDetail("freezeDescription")}
        recipientsCount={passedRecipientIds(preflight).length}
        sender={mailboxAddress || "—"}
        purpose="marketing"
        contentPreview={{
          subject: selectedVersionForPreview?.subject ?? undefined,
        }}
        confirmText={tDetail("freezeConfirm")}
        confirmLabel={tDetail("freezeConfirm")}
        destructive={false}
        pending={pending === "freeze"}
        onConfirm={freeze}
      />
    </div>
  );
}

function PreflightSummary({
  result,
  locale,
}: {
  result: PreflightResult;
  locale: string;
}) {
  const t = useTranslations("campaigns.preflight");
  const tReason = useTranslations("campaigns.reasonCodes");
  const summary = summarizePreflight(result);
  return (
    <div className="flex flex-col gap-2 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant="success-subtle">
          {t("passed", { count: summary.passed })}
        </Badge>
        <Badge variant="amber-subtle">
          {t("excluded", { count: summary.excluded })}
        </Badge>
        {summary.checkedAt ? (
          <span className="text-2xs text-muted-foreground">
            {t("checkedAt", { time: formatDateTime(summary.checkedAt, locale) })}
          </span>
        ) : null}
      </div>
      {summary.byReason.size > 0 ? (
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-2xs text-muted-foreground">
            {t("excludedByReason")}:
          </span>
          {[...summary.byReason.entries()].map(([code, count]) => (
            <Badge key={code} variant="secondary">
              {tReason.has(code) ? tReason(code) : code} ×{count}
            </Badge>
          ))}
        </div>
      ) : null}
      {summary.globalBlocking.length > 0 ? (
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-2xs text-muted-foreground">
            {t("globalBlocks")}:
          </span>
          {summary.globalBlocking.map((code) => (
            <Badge key={code} variant="rose-subtle">
              {tReason.has(code) ? tReason(code) : code}
            </Badge>
          ))}
        </div>
      ) : null}
      <p className="text-2xs text-muted-foreground">{t("notAuthorization")}</p>
    </div>
  );
}
