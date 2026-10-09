"use client";

import { useTranslations } from "next-intl";

import { cn } from "@/lib/utils";

export interface ConflictField {
  key: string;
  label: string;
  base: string;
  server: string;
  mine: string;
}

export type ConflictChoice = "server" | "mine";

export interface ConflictDiffProps {
  /** 412 VERSION_CONFLICT 시 base / server / mine 3-way 비교 (§7.2). */
  fields: ConflictField[];
  choices: Record<string, ConflictChoice>;
  onChoiceChange: (key: string, choice: ConflictChoice) => void;
  className?: string;
}

/**
 * 동시 편집 충돌 뷰 — 저장 실패를 감추지 않고 변경 전·현재 서버 값·내 변경을
 * 나란히 보여 주고 필드별로 선택하게 한다.
 */
export function ConflictDiff({
  fields,
  choices,
  onChoiceChange,
  className,
}: ConflictDiffProps) {
  const t = useTranslations("common.conflict");

  return (
    <div
      data-slot="conflict-diff"
      className={cn("overflow-x-auto rounded-2xl border border-border", className)}
    >
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border bg-app-panel-muted text-left text-xs text-muted-foreground">
            <th scope="col" className="px-3 py-2 font-medium">
              {t("field")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("base")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("server")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("mine")}
            </th>
          </tr>
        </thead>
        <tbody>
          {fields.map((field) => {
            const serverChanged = field.server !== field.base;
            const mineChanged = field.mine !== field.base;
            const conflicted = serverChanged && mineChanged && field.server !== field.mine;
            const choice = choices[field.key] ?? (conflicted ? "server" : "mine");
            return (
              <tr key={field.key} className="border-b border-border/60 last:border-b-0">
                <th scope="row" className="px-3 py-2 text-left font-medium">
                  {field.label}
                </th>
                <td className="px-3 py-2 text-muted-foreground">{field.base}</td>
                {(["server", "mine"] as const).map((side) => {
                  const value = side === "server" ? field.server : field.mine;
                  const changed =
                    side === "server" ? serverChanged : mineChanged;
                  const selected = choice === side;
                  return (
                    <td key={side} className="px-3 py-2">
                      <label
                        className={cn(
                          "flex cursor-pointer items-start gap-2 rounded-lg border px-2 py-1.5",
                          selected ? "border-ring bg-accent/50" : "border-transparent",
                          changed ? "text-foreground" : "text-muted-foreground"
                        )}
                      >
                        <input
                          type="radio"
                          name={`conflict-${field.key}`}
                          value={side}
                          checked={selected}
                          onChange={() => onChoiceChange(field.key, side)}
                          className="mt-0.5 accent-primary"
                        />
                        <span className="whitespace-pre-wrap break-words">
                          {value}
                        </span>
                      </label>
                    </td>
                  );
                })}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
