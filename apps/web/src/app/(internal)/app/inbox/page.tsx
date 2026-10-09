import { useTranslations } from "next-intl";
import { InfoIcon } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Card } from "@/components/ui/card";

export default function InboxPage() {
  const t = useTranslations("screens.inbox");
  const ts = useTranslations("screens");
  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />
      <Alert variant="info">
        <InfoIcon />
        <AlertTitle>{ts("placeholderTitle")}</AlertTitle>
        <AlertDescription>{ts("placeholderDescription")}</AlertDescription>
      </Alert>
      {/* 목록|본문|CRM 보조 3분할 자리 */}
      <div className="grid gap-4 lg:grid-cols-[minmax(0,18rem)_minmax(0,1fr)_minmax(0,16rem)]">
        <Card className="min-h-72 p-3">
          <EmptyState title={t("emptyTitle")} compact />
        </Card>
        <Card className="hidden min-h-72 items-center justify-center p-6 lg:flex">
          <EmptyState
            title={t("emptyTitle")}
            description={t("emptyDescription")}
            compact
          />
        </Card>
        <Card className="hidden min-h-72 p-3 lg:block">
          <EmptyState title={t("emptyTitle")} compact />
        </Card>
      </div>
    </div>
  );
}
