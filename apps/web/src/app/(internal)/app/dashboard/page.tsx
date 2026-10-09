import { useTranslations } from "next-intl";
import {
  ChartColumnIcon,
  ClockIcon,
  HelpCircleIcon,
  InboxIcon,
  CalendarClockIcon,
  ShieldCheckIcon,
} from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { InfoIcon } from "lucide-react";

const KPI = [
  { key: "kpiReplies", icon: InboxIcon },
  { key: "kpiApprovals", icon: ShieldCheckIcon },
  { key: "kpiMeetings", icon: CalendarClockIcon },
  { key: "kpiUnknown", icon: HelpCircleIcon },
  { key: "kpiSyncLag", icon: ClockIcon },
  { key: "kpiCohort", icon: ChartColumnIcon },
] as const;

export default function DashboardPage() {
  const t = useTranslations("screens.dashboard");
  const ts = useTranslations("screens");
  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />
      <Alert variant="info">
        <InfoIcon />
        <AlertTitle>{ts("placeholderTitle")}</AlertTitle>
        <AlertDescription>{ts("placeholderDescription")}</AlertDescription>
      </Alert>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 xl:grid-cols-6">
        {KPI.map(({ key, icon: Icon }) => (
          <Card key={key} className="gap-2 py-3">
            <CardHeader className="pb-1">
              <CardTitle className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <Icon className="size-3.5" aria-hidden />
                {t(key)}
              </CardTitle>
            </CardHeader>
            <CardContent>
              <Skeleton className="h-7 w-10" />
            </CardContent>
          </Card>
        ))}
      </div>
      <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
    </div>
  );
}
