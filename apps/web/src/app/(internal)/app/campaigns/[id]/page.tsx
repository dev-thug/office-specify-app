import { useTranslations } from "next-intl";
import { CheckIcon, InfoIcon } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";

export default function CampaignDetailPage() {
  const t = useTranslations("screens.campaignDetail");
  const ts = useTranslations("screens");
  const steps = t("steps").split("|");
  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />
      <Alert variant="info">
        <InfoIcon />
        <AlertTitle>{ts("placeholderTitle")}</AlertTitle>
        <AlertDescription>{ts("placeholderDescription")}</AlertDescription>
      </Alert>
      {/* §7.5 캠페인 6단계 */}
      <ol className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {steps.map((step, i) => (
          <li
            key={step}
            className="flex items-center gap-2.5 rounded-2xl border border-border bg-card px-3.5 py-3 text-sm"
          >
            <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
              {i === 0 ? <CheckIcon className="size-3.5" /> : i + 1}
            </span>
            <span className="font-medium">{step.replace(/^\d+\.\s*/, "")}</span>
          </li>
        ))}
      </ol>
      <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
    </div>
  );
}
