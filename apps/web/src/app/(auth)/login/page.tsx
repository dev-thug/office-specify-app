import { useTranslations } from "next-intl";
import { Suspense } from "react";

import { LoginForm } from "./login-form";

export default function LoginPage() {
  const t = useTranslations("auth.login");
  return (
    <div className="rounded-3xl border border-border bg-card p-6 shadow-sm">
      <h1 className="text-xl font-semibold">{t("title")}</h1>
      <p className="mt-1 text-sm text-muted-foreground">{t("description")}</p>
      <div className="mt-5">
        <Suspense>
          <LoginForm />
        </Suspense>
      </div>
    </div>
  );
}
