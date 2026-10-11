"use client";

/**
 * 발송 채널 섹션 — 정책 게이트(policy_status)가 활성화를 결정한다.
 * 토글은 게이트를 우회할 수 없다: policy_status !== 'allowed' 면 스위치를
 * 비활성화하고, PATCH 도 If-Match 로 보내 412 를 표면에 드러낸다.
 */
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
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
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  createChannelReview,
  createDeliveryChannel,
  patchDeliveryChannel,
  useChannelReviews,
  useDeliveryChannels,
  usePrincipals,
  useMailAccounts,
  type ChannelPurpose,
  type ChannelReview,
  type ChannelReviewResult,
  type DeliveryChannel,
  type DeliveryProvider,
} from "@/lib/api/settings";
import { channelToggleDisabled } from "@/features/operations/gate-rules";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDateTime } from "@/features/settings/format";
import { useIsAdmin } from "@/features/settings/use-me";

const PURPOSES: ChannelPurpose[] = ["correspondence", "marketing", "transactional"];
const PROVIDERS: DeliveryProvider[] = [
  "zoho_mail",
  "delivery_provider",
  "transactional_provider",
];

export function DeliveryChannelsSection() {
  const t = useTranslations("operations.integrations");
  const tOps = useTranslations("operations");
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();

  const channels = useDeliveryChannels();
  const rows = channels.data?.pages.flatMap((p) => p.data) ?? [];
  const page = channels.data?.pages.at(-1)?.page;

  const [reviewsFor, setReviewsFor] = useState<DeliveryChannel | null>(null);
  const [reviewFormFor, setReviewFormFor] = useState<DeliveryChannel | null>(null);
  const [createOpen, setCreateOpen] = useState(false);

  const toggle = useMutation({
    mutationFn: ({ channel, enabled }: { channel: DeliveryChannel; enabled: boolean }) =>
      patchDeliveryChannel(
        channel.id,
        { enabled },
        { etag: channel.version, idempotencyKey: idempotencyKey() }
      ),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["operations", "delivery-channels"] }),
    onError: (err) => {
      // 412 포함 — 최신 상태로 갱신해 낙관적 표시를 되돌린다.
      queryClient.invalidateQueries({ queryKey: ["operations", "delivery-channels"] });
      toast.error(apiErrorText(tOps, err));
    },
  });

  const columns: ColumnDef<DeliveryChannel>[] = [
    {
      key: "name",
      header: t("channel.name"),
      cell: (row) => <span className="font-medium">{row.name}</span>,
    },
    {
      key: "provider",
      header: t("channel.provider"),
      cell: (row) => (
        <span className="text-xs">
          {t.has(`provider.${row.provider}`) ? t(`provider.${row.provider}`) : row.provider}
        </span>
      ),
    },
    {
      key: "purpose",
      header: t("channel.purpose"),
      cell: (row) => (
        <span className="text-xs">
          {t.has(`purpose.${row.purpose}`) ? t(`purpose.${row.purpose}`) : row.purpose}
        </span>
      ),
    },
    {
      key: "policy",
      header: t("channel.policy"),
      cell: (row) => <StatusBadge status={row.policy_status} />,
    },
    {
      key: "enabled",
      header: t("channel.enabled"),
      cell: (row) => (
        <Switch
          checked={row.enabled}
          disabled={channelToggleDisabled(row, {
            isAdmin,
            pending: toggle.isPending,
          })}
          onCheckedChange={(enabled) => toggle.mutate({ channel: row, enabled })}
          aria-label={`${t("channel.enabled")} — ${row.name}`}
          title={row.policy_status !== "allowed" ? t("channel.gateNotice") : undefined}
        />
      ),
    },
    {
      key: "dailyCap",
      header: t("channel.dailyCap"),
      cell: (row) => (
        <span className="tabular-nums text-xs">
          {row.internal_daily_cap ?? t("channel.noCap")}
        </span>
      ),
    },
    {
      key: "actions",
      header: "",
      cell: (row) => (
        <div className="flex items-center gap-1">
          <Button type="button" variant="ghost" size="xs" onClick={() => setReviewsFor(row)}>
            {t("channel.reviews")}
          </Button>
          {isAdmin ? (
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => setReviewFormFor(row)}
            >
              {t("channel.addReview")}
            </Button>
          ) : null}
        </div>
      ),
    },
  ];

  return (
    <section className="flex flex-col gap-3" aria-labelledby="integrations-channels">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="integrations-channels" className="text-sm font-medium">
          {t("channelSection")}
        </h2>
        {isAdmin ? (
          <Button type="button" variant="outline" size="sm" onClick={() => setCreateOpen(true)}>
            {t("channel.create")}
          </Button>
        ) : null}
      </div>
      <p className="text-2xs text-muted-foreground">
        {t("channel.gateNotice")} {t("channel.zohoNotice")}
      </p>
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={channels.isPending}
        error={channels.error}
        onRetry={() => channels.refetch()}
        page={page}
        onLoadMore={() => channels.fetchNextPage()}
        loadingMore={channels.isFetchingNextPage}
        emptyState={
          <EmptyState
            title={t("channel.emptyTitle")}
            description={t("channel.emptyDescription")}
          />
        }
        caption={t("channelSection")}
      />

      <ReviewsDialog
        channel={reviewsFor}
        onOpenChange={(open) => !open && setReviewsFor(null)}
      />
      <ReviewFormDialog
        channel={reviewFormFor}
        onOpenChange={(open) => !open && setReviewFormFor(null)}
      />
      <CreateChannelDialog open={createOpen} onOpenChange={setCreateOpen} />
    </section>
  );
}

// ── 검토 이력 읽기 ────────────────────────────────────────────

function ReviewsDialog({
  channel,
  onOpenChange,
}: {
  channel: DeliveryChannel | null;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("operations.integrations.reviews");
  const tc = useTranslations("common");
  const locale = useLocale();
  const principals = usePrincipals();
  const reviews = useChannelReviews(channel?.id);
  const rows = reviews.data?.pages.flatMap((p) => p.data) ?? [];
  const page = reviews.data?.pages.at(-1)?.page;

  const columns: ColumnDef<ChannelReview>[] = [
    {
      key: "result",
      header: t("result"),
      cell: (row) => <StatusBadge status={row.result} />,
    },
    {
      key: "reviewedBy",
      header: t("reviewedBy"),
      cell: (row) => {
        const principal = principals.data?.pages
          .flatMap((page) => page.data)
          .find((item) => item.id === row.reviewed_by);
        return <span>{principal?.name?.trim() || principal?.display_name?.trim() || tc("nameUnavailable")}</span>;
      },
    },
    {
      key: "reviewedAt",
      header: t("reviewedAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">{formatDateTime(row.reviewed_at, locale)}</span>
      ),
    },
    {
      key: "expiresAt",
      header: t("expiresAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">{formatDateTime(row.expires_at, locale)}</span>
      ),
    },
    {
      key: "notes",
      header: t("notes"),
      cell: (row) => <span className="text-xs">{row.notes ?? "—"}</span>,
    },
  ];

  return (
    <Dialog open={!!channel} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{channel?.name}</DialogDescription>
        </DialogHeader>
        <DataTable
          columns={columns}
          rows={rows}
          rowKey={(row) => row.id}
          loading={reviews.isPending}
          error={reviews.error}
          onRetry={() => reviews.refetch()}
          page={page}
          onLoadMore={() => reviews.fetchNextPage()}
          loadingMore={reviews.isFetchingNextPage}
          emptyState={
            <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} compact />
          }
          caption={t("title")}
        />
      </DialogContent>
    </Dialog>
  );
}

// ── 검토 추가 (append-only) ──────────────────────────────────

function ReviewFormDialog({
  channel,
  onOpenChange,
}: {
  channel: DeliveryChannel | null;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("operations.integrations.review");
  const tOps = useTranslations("operations");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const idemRef = useRef(idempotencyKey());

  const [result, setResult] = useState<ChannelReviewResult>("allowed");
  const [policyUrl, setPolicyUrl] = useState("");
  const [evidenceRef, setEvidenceRef] = useState("");
  const [notes, setNotes] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [error, setError] = useState<string | null>(null);

  const submit = useMutation({
    mutationFn: () =>
      createChannelReview(
        channel!.id,
        {
          result,
          policy_url: policyUrl.trim() || undefined,
          contract_evidence_ref: evidenceRef.trim() || undefined,
          notes: notes.trim() || undefined,
          expires_at: expiresAt ? new Date(expiresAt).toISOString() : undefined,
        },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("success"));
      idemRef.current = idempotencyKey();
      setError(null);
      queryClient.invalidateQueries({ queryKey: ["operations", "delivery-channels"] });
      onOpenChange(false);
    },
    onError: (err) => {
      setError(apiErrorText(tOps, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (submit.isPending) return;
    submit.mutate();
  }

  return (
    <Dialog open={!!channel} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>
            {channel?.name} — {t("appendOnly")}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="review-result">{t("result")}</Label>
            <Select value={result} onValueChange={(v) => setResult(v as ChannelReviewResult)}>
              <SelectTrigger id="review-result" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="allowed">{t("resultAllowed")}</SelectItem>
                <SelectItem value="blocked">{t("resultBlocked")}</SelectItem>
                <SelectItem value="unverified">{t("resultUnverified")}</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="review-policy-url">{t("policyUrl")}</Label>
            <Input
              id="review-policy-url"
              type="url"
              value={policyUrl}
              onChange={(e) => setPolicyUrl(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="review-evidence">{t("evidenceRef")}</Label>
            <Input
              id="review-evidence"
              value={evidenceRef}
              onChange={(e) => setEvidenceRef(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="review-notes">{t("notes")}</Label>
            <Textarea
              id="review-notes"
              rows={2}
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="review-expires">{t("expiresAt")}</Label>
            <Input
              id="review-expires"
              type="datetime-local"
              value={expiresAt}
              onChange={(e) => setExpiresAt(e.target.value)}
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
            <Button type="submit" disabled={submit.isPending}>
              {t("submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

// ── 채널 생성 ────────────────────────────────────────────────

function CreateChannelDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const t = useTranslations("operations.integrations");
  const tOps = useTranslations("operations");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const idemRef = useRef(idempotencyKey());
  const mailAccounts = useMailAccounts();
  const accounts = mailAccounts.data?.pages.flatMap((p) => p.data) ?? [];

  const [name, setName] = useState("");
  const [provider, setProvider] = useState<DeliveryProvider>("delivery_provider");
  const [purpose, setPurpose] = useState<ChannelPurpose>("correspondence");
  const [mailAccountId, setMailAccountId] = useState("");
  const [senderIdentityId, setSenderIdentityId] = useState("");
  const [cap, setCap] = useState("");
  const [error, setError] = useState<string | null>(null);

  const account = accounts.find((a) => a.id === mailAccountId);
  // DDL CHECK: zoho_mail 채널은 correspondence 외 목적을 활성화할 수 없다 — 생성도 막는다.
  const zohoForbidden = provider === "zoho_mail" && purpose !== "correspondence";

  const submit = useMutation({
    mutationFn: () =>
      createDeliveryChannel(
        {
          name: name.trim(),
          provider,
          purpose,
          mail_account_id: mailAccountId,
          sender_identity_id: senderIdentityId.trim(),
          connection_id: account?.connection_id,
          internal_daily_cap: cap ? Number(cap) : undefined,
        },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      idemRef.current = idempotencyKey();
      setError(null);
      queryClient.invalidateQueries({ queryKey: ["operations", "delivery-channels"] });
      onOpenChange(false);
    },
    onError: (err) => {
      setError(apiErrorText(tOps, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  const valid =
    name.trim().length > 0 &&
    mailAccountId.length > 0 &&
    senderIdentityId.trim().length > 0 &&
    !zohoForbidden;

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!valid || submit.isPending) return;
    submit.mutate();
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("channel.createTitle")}</DialogTitle>
          <DialogDescription>{t("channel.gateNotice")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="ch-name">{t("channel.nameLabel")}</Label>
            <Input id="ch-name" required value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="ch-provider">{t("channel.providerLabel")}</Label>
            <Select value={provider} onValueChange={(v) => setProvider(v as DeliveryProvider)}>
              <SelectTrigger id="ch-provider" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PROVIDERS.map((p) => (
                  <SelectItem key={p} value={p}>
                    {t(`provider.${p}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="ch-purpose">{t("channel.purposeLabel")}</Label>
            <Select value={purpose} onValueChange={(v) => setPurpose(v as ChannelPurpose)}>
              <SelectTrigger id="ch-purpose" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PURPOSES.map((p) => (
                  <SelectItem key={p} value={p}>
                    {t(`purpose.${p}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {zohoForbidden ? (
              <p role="alert" className="text-2xs text-destructive">
                {t("channel.zohoNotice")}
              </p>
            ) : null}
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="ch-account">{t("channel.mailAccount")}</Label>
            <Select value={mailAccountId} onValueChange={setMailAccountId}>
              <SelectTrigger id="ch-account" className="w-full">
                <SelectValue placeholder={t("channel.mailAccount")} />
              </SelectTrigger>
              <SelectContent>
                {accounts.map((a) => (
                  <SelectItem key={a.id} value={a.id}>
                    {a.primary_address}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="ch-sender">{t("channel.senderIdentity")}</Label>
            <Input
              id="ch-sender"
              required
              value={senderIdentityId}
              onChange={(e) => setSenderIdentityId(e.target.value)}
              aria-describedby="ch-sender-hint"
            />
            <p id="ch-sender-hint" className="text-2xs text-muted-foreground">
              {t("channel.senderIdentityHint")}
            </p>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="ch-cap">{t("channel.capLabel")}</Label>
            <Input
              id="ch-cap"
              type="number"
              min={1}
              value={cap}
              onChange={(e) => setCap(e.target.value)}
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
            <Button type="submit" disabled={!valid || submit.isPending}>
              {t("channel.createSubmit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
