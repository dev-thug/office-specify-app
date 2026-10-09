import Image from "next/image";
import Link from "next/link";
import { useTranslations } from "next-intl";

import { buttonVariants } from "@/components/ui/button";

export default function NotFound() {
  const t = useTranslations();
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center gap-4 bg-background px-6 text-center">
      <Image src="/logo.png" alt={t("app.name")} width={56} height={56} className="rounded-2xl" />
      <h1 className="text-xl font-semibold">{t("common.notFound.title")}</h1>
      <p className="max-w-sm text-sm text-muted-foreground">
        {t("common.notFound.description")}
      </p>
      <Link href="/app/dashboard" className={buttonVariants({ size: "sm" })}>
        {t("common.notFound.back")}
      </Link>
    </div>
  );
}
