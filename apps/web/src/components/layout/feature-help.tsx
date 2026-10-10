"use client";

import { BookOpenIcon, CircleHelpIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { flattenNav } from "@/lib/nav";

function FeatureInstructions({ featureKey }: { featureKey: string }) {
  const t = useTranslations("nav.help");
  const steps = t.raw(`features.${featureKey}.steps`) as string[];
  return <div className="grid gap-4 text-sm leading-relaxed">
    <div><h3 className="mb-1 font-medium">{t("when")}</h3><p className="text-muted-foreground">{t(`features.${featureKey}.when`)}</p></div>
    <div><h3 className="mb-2 font-medium">{t("steps")}</h3>
      <ol className="list-decimal space-y-2 pl-5 text-muted-foreground">{steps.map((step) => <li key={step}>{step}</li>)}</ol>
    </div>
  </div>;
}

/** The help button is a sibling of navigation links, so opening help never navigates. */
export function FeatureHelp({ featureKey }: { featureKey: string }) {
  const t = useTranslations("nav");
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const feature = t(featureKey);
  return <>
    <TooltipProvider delayDuration={250}>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button ref={trigger} type="button" size="icon-sm" variant="ghost" className="size-7 shrink-0 rounded-full text-muted-foreground"
            aria-label={t("help.label", { feature })} aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen(true)}>
            <CircleHelpIcon className="size-3.5" aria-hidden="true" />
          </Button>
        </TooltipTrigger>
        <TooltipContent variant="card" side="right" sideOffset={6} className="max-w-[min(20rem,calc(100vw-2rem))] p-3 text-left leading-relaxed">
          <p className="font-medium">{feature}</p>
          <p className="mt-1">{t(`help.features.${featureKey}.summary`)}</p>
          <p className="mt-2 text-muted-foreground">{t("help.more")}</p>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent onCloseAutoFocus={(event) => { event.preventDefault(); trigger.current?.focus(); }} className="max-h-[85dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("help.label", { feature })}</DialogTitle>
          <DialogDescription className="leading-relaxed">{t(`help.features.${featureKey}.summary`)}</DialogDescription>
        </DialogHeader>
        <FeatureInstructions featureKey={featureKey} />
      </DialogContent>
    </Dialog>
  </>;
}

export function UsageGuide() {
  const t = useTranslations("nav");
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const items = flattenNav();
  const workflow = t.raw("help.workflow") as { title: string; body: string; key: string }[];
  return <>
    <Button ref={trigger} type="button" variant="ghost" size="sm" onClick={() => setOpen(true)} aria-label={t("help.guide")} aria-haspopup="dialog">
      <BookOpenIcon className="size-4" aria-hidden="true" /><span className="hidden sm:inline">{t("help.guide")}</span>
    </Button>
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent onCloseAutoFocus={(event) => { event.preventDefault(); trigger.current?.focus(); }} className="max-h-[85dvh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader><DialogTitle>{t("help.guide")}</DialogTitle><DialogDescription>{t("help.guideDescription")}</DialogDescription></DialogHeader>
        <section aria-label={t("help.start")}>
          <h3 className="mb-3 font-medium">{t("help.start")}</h3>
          <ol className="grid gap-3 sm:grid-cols-2">{workflow.map((step) => {
            const item = items.find((entry) => entry.key === step.key);
            return <li key={step.key} className="rounded-xl border bg-muted/30 p-4">
              <h4 className="text-sm font-medium">{step.title}</h4><p className="mt-2 text-sm leading-relaxed text-muted-foreground">{step.body}</p>
              {item ? <Link href={item.href} onClick={() => setOpen(false)} className="mt-3 inline-block text-sm underline underline-offset-4">{t("help.open", {feature: t(item.key)})}</Link> : null}
            </li>;
          })}</ol>
        </section>
        <section aria-label={t("help.allFeatures")} className="mt-3">
          <h3 className="mb-3 font-medium">{t("help.allFeatures")}</h3>
          <div className="divide-y rounded-xl border px-4">{items.map((item) => <details key={item.key} className="py-3">
            <summary className="cursor-pointer text-sm font-medium">{t(item.key)}</summary>
            <div className="mt-3 grid gap-3">
              <p className="text-sm leading-relaxed text-muted-foreground">{t(`help.features.${item.key}.summary`)}</p>
              <FeatureInstructions featureKey={item.key} />
              <Link href={item.href} onClick={() => setOpen(false)} className="w-fit text-sm underline underline-offset-4">{t("help.open", {feature: t(item.key)})}</Link>
            </div>
          </details>)}</div>
        </section>
      </DialogContent>
    </Dialog>
  </>;
}
