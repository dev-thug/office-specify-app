"use client";

import { useTranslations } from "next-intl";
import { useRouter } from "next/navigation";
import { useEffect } from "react";

import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { flattenNav } from "@/lib/nav";

export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations();
  const router = useRouter();

  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("common.commandPalette.title")}
      description={t("common.commandPalette.placeholder")}
    >
      <CommandInput placeholder={t("common.commandPalette.placeholder")} />
      <CommandList>
        <CommandEmpty>{t("common.commandPalette.empty")}</CommandEmpty>
        <CommandGroup heading={t("common.commandPalette.navigation")}>
          {flattenNav().map((item) => (
            <CommandItem
              key={item.href}
              value={`${t(`nav.${item.key}`)} ${item.href} ${item.searchTerms?.join(" ") ?? ""}`}
              onSelect={() => {
                router.push(item.href);
                onOpenChange(false);
              }}
            >
              <item.icon />
              <span>{t(`nav.${item.key}`)}</span>
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}

/** ⌘K / Ctrl+K 로 팔레트를 연다. */
export function useCommandPaletteShortcut(onOpen: () => void) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        onOpen();
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onOpen]);
}
