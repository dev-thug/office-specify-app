"use client";

import { useTranslations } from "next-intl";
import { useState } from "react";

import { Button } from "@/components/ui/button";
import { CheckCircle2Icon } from "lucide-react";

export function UnsubscribeConfirm() {
  const t = useTranslations("public.unsubscribe");
  const [state, setState] = useState<"idle" | "pending" | "done">("idle");

  if (state === "done") {
    return (
      <div className="flex flex-col items-center gap-3 py-10 text-center">
        <CheckCircle2Icon className="size-10 text-teal" aria-hidden />
        <h1 className="text-xl font-semibold">{t("doneTitle")}</h1>
        <p className="max-w-sm text-sm text-muted-foreground">
          {t("doneDescription")}
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-5">
      <div>
        <h1 className="text-2xl font-semibold">{t("title")}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{t("description")}</p>
      </div>
      <div className="rounded-2xl border border-border bg-card px-4 py-3 text-sm">
        <span className="text-muted-foreground">{t("scope")}: </span>
        {/* 실제 범위는 /public/unsubscribe/:token 확인 응답에서 채운다 */}
        <span className="font-medium">—</span>
      </div>
      <Button
        type="button"
        variant="destructive"
        className="w-full"
        disabled={state === "pending"}
        onClick={() => setState("done")}
      >
        {state === "pending" ? t("confirming") : t("confirm")}
      </Button>
    </div>
  );
}
