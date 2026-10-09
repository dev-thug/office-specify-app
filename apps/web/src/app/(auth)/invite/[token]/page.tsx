import { useTranslations } from "next-intl";
import { Suspense } from "react";

import { InviteForm } from "./invite-form";

export default function InvitePage() {
  const t = useTranslations("auth.invite");
  return (
    <div className="rounded-3xl border border-border bg-card p-6 shadow-sm">
      <h1 className="text-xl font-semibold">{t("title")}</h1>
      <div className="mt-5">
        <Suspense>
          <InviteForm />
        </Suspense>
      </div>
    </div>
  );
}
