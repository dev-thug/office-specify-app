"use client";

import { useMutation, useQuery } from "@tanstack/react-query";
import { ArrowLeft, Plus } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState, type FormEvent } from "react";

import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { useIdempotencyKey } from "@/features/campaigns/use-idempotency-key";
import {
  createSalesCampaign,
  getSalesCampaignSender,
} from "@/lib/api/sales-campaigns";
import {
  MEETING_TYPES_PATH,
  type MeetingType,
} from "@/lib/api/meetings";
import { ApiError } from "@/lib/api/problem";
import { useCursorList } from "@/lib/api/use-cursor-list";

export function PersonalizedCampaignCreate() {
  const t = useTranslations("campaigns.personalized.create");
  const router = useRouter();
  const operation = useIdempotencyKey();
  const [name, setName] = useState("");
  const [objective, setObjective] = useState("");
  const [meetingTypeId, setMeetingTypeId] = useState("");
  const [error, setError] = useState<string | null>(null);

  const senderQuery = useQuery({
    queryKey: ["sales-campaigns", "sender"],
    queryFn: async () => (await getSalesCampaignSender()).data,
    retry: false,
  });
  const meetingTypesQuery = useCursorList<MeetingType>({
    queryKey: ["meetings", "types", "sales-campaign-create"],
    path: MEETING_TYPES_PATH,
    limit: 100,
  });
  const meetingTypes =
    meetingTypesQuery.data?.pages.flatMap((page) => page.data) ?? [];
  const activeMeetingTypes = meetingTypes.filter((type) => type.status === "active");
  const sender = senderQuery.data?.sender;
  const senderReady = Boolean(sender?.ready);

  const create = useMutation({
    mutationFn: async () =>
      createSalesCampaign(
        {
          name: name.trim(),
          objective: objective.trim() || null,
          meeting_type_id: meetingTypeId || null,
        },
        { idempotencyKey: operation.key() }
      ),
    onSuccess: (result) => {
      operation.settle();
      const campaignId = result.data.id;
      router.push(`/app/campaigns/${encodeURIComponent(campaignId)}`);
    },
    onError: (cause) => {
      operation.settle(cause);
      const detail = cause instanceof ApiError ? cause.problem.detail : null;
      setError(detail ? `${t("error")} (${detail})` : t("error"));
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setError(null);
    if (!name.trim() || !senderReady || create.isPending) return;
    create.mutate();
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-5 px-6 py-6">
      <Link
        href="/app/campaigns"
        className="inline-flex w-fit items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft aria-hidden className="size-4" />
        {t("back")}
      </Link>
      <PageHeader title={t("title")} description={t("description")} />

      {senderQuery.isPending ? (
        <Skeleton className="h-20 rounded-2xl" />
      ) : senderReady && sender ? (
        <div className="rounded-2xl border border-border bg-app-panel-muted px-4 py-3">
          <p className="text-xs text-muted-foreground">{t("sender")}</p>
          <p className="mt-1 text-sm font-medium">
            {sender.profile_name} · {sender.from_name} &lt;{sender.from_address}&gt;
          </p>
        </div>
      ) : (
        <Alert variant="warning">
          <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
            <span>{t("senderMissing")}</span>
            <Button asChild size="xs" variant="link">
              <Link href="/app/settings/mail-accounts">{t("openSettings")}</Link>
            </Button>
          </AlertDescription>
        </Alert>
      )}

      <form
        onSubmit={(event) => void submit(event)}
        className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4"
      >
        <div className="grid gap-1.5">
          <Label htmlFor="sales-campaign-name">{t("name")}</Label>
          <Input
            id="sales-campaign-name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={t("namePlaceholder")}
            required
            maxLength={200}
          />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="sales-campaign-objective">{t("objective")}</Label>
          <Textarea
            id="sales-campaign-objective"
            value={objective}
            onChange={(event) => setObjective(event.target.value)}
            placeholder={t("objectivePlaceholder")}
            rows={3}
            maxLength={2000}
          />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="sales-campaign-meeting-type">{t("meetingType")}</Label>
          <Select value={meetingTypeId || "none"} onValueChange={(value) => setMeetingTypeId(value === "none" ? "" : value)}>
            <SelectTrigger id="sales-campaign-meeting-type" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">{t("noMeetingType")}</SelectItem>
              {activeMeetingTypes.map((type) => (
                <SelectItem key={type.id} value={type.id}>
                  {type.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {meetingTypesQuery.isSuccess && activeMeetingTypes.length === 0 ? (
            <p className="text-2xs text-muted-foreground">
              {t("meetingTypeEmpty")}{" "}
              <Link
                href="/app/meeting-types"
                className="font-medium text-foreground underline underline-offset-2"
              >
                {t("createMeetingType")}
              </Link>
            </p>
          ) : null}
          {meetingTypesQuery.isError ? (
            <p role="alert" className="text-2xs text-destructive">
              {meetingTypesQuery.error.message}
            </p>
          ) : null}
        </div>
        {error ? (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        ) : null}
        <div className="flex justify-end">
          <Button type="submit" disabled={!name.trim() || !senderReady || create.isPending}>
            <Plus aria-hidden />
            {create.isPending ? t("saving") : t("save")}
          </Button>
        </div>
      </form>
    </div>
  );
}
