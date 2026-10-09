"use client";

import { useTranslations } from "next-intl";
import Image from "next/image";

import { cn } from "@/lib/utils";

interface LogoProps {
  size?: "sm" | "md";
  withWordmark?: boolean;
  className?: string;
}

/** 보라 브랜드 색은 로고 이미지에만 있다 — 워드마크는 잉크 텍스트. */
export function Logo({ size = "md", withWordmark = true, className }: LogoProps) {
  const t = useTranslations("app");
  const px = size === "sm" ? 22 : 26;
  return (
    <span className={cn("flex items-center gap-2 select-none", className)}>
      <Image
        src="/logo.png"
        alt={t("name")}
        width={px}
        height={px}
        className="shrink-0 rounded-md"
        priority
      />
      {withWordmark ? (
        <span className="text-ui font-semibold text-foreground">{t("name")}</span>
      ) : null}
    </span>
  );
}
