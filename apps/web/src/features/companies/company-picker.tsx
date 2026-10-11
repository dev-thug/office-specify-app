"use client";

/**
 * 회사 검색 선택 — GET /companies?q= 커서 목록 첫 페이지를 보여 준다.
 * 담당자 폼의 회사 필드와 병합 대화상자의 소스 선택에서 함께 쓴다.
 */
import { useEffect, useState } from "react";
import { useTranslations } from "next-intl";
import { Check, ChevronsUpDown } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import {
  companiesPath,
  companyListParams,
  type Company,
} from "@/lib/api/companies";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { cn } from "@/lib/utils";

interface CompanyPickerProps {
  value: Company | null;
  onChange: (company: Company | null) => void;
  placeholder: string;
  noneLabel: string;
  emptyLabel: string;
  /** 결과에서 제외할 회사 id (자기 자신). */
  excludeId?: string;
  disabled?: boolean;
}

export function CompanyPicker({
  value,
  onChange,
  placeholder,
  noneLabel,
  emptyLabel,
  excludeId,
  disabled,
}: CompanyPickerProps) {
  const tCommon = useTranslations("common");
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");

  useEffect(() => {
    const id = setTimeout(() => setDebounced(query.trim()), 250);
    return () => clearTimeout(id);
  }, [query]);

  const list = useCursorList<Company>({
    queryKey: ["companies", "picker", debounced],
    path: companiesPath(),
    params: companyListParams({ q: debounced }),
    limit: 20,
    enabled: open,
  });

  const rows = (list.data?.pages ?? [])
    .flatMap((p) => p.data ?? [])
    .filter((c) => c.id !== excludeId);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-label={placeholder}
          aria-expanded={open}
          disabled={disabled}
          className="w-full justify-between font-normal"
        >
          <span className="truncate">
            {value ? value.name : placeholder}
          </span>
          <ChevronsUpDown className="size-4 shrink-0 opacity-50" aria-hidden />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-72 p-2" align="start">
        <Input
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={placeholder}
          aria-label={placeholder}
          className="mb-2"
        />
        <ul className="max-h-56 overflow-y-auto">
          <li>
            <button
              type="button"
              className="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-sm hover:bg-accent"
              onClick={() => {
                onChange(null);
                setOpen(false);
              }}
            >
              {noneLabel}
            </button>
          </li>
          {list.isPending ? (
            <li className="p-2">
              <Skeleton className="h-6 w-full" />
            </li>
          ) : (
            rows.map((c) => (
              <li key={c.id}>
                <button
                  type="button"
                  className={cn(
                    "flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-sm hover:bg-accent",
                    value?.id === c.id && "bg-accent/60"
                  )}
                  onClick={() => {
                    onChange(c);
                    setOpen(false);
                  }}
                >
                  <Check
                    className={cn(
                      "size-3.5 shrink-0",
                      value?.id === c.id ? "opacity-100" : "opacity-0"
                    )}
                    aria-hidden
                  />
                  <span className="min-w-0 flex-1 truncate">{c.name}</span>
                  {c.domain ? (
                    <span className="text-2xs text-muted-foreground">
                      {c.domain}
                    </span>
                  ) : null}
                </button>
              </li>
            ))
          )}
          {list.hasNextPage ? (
            <li>
              <button
                type="button"
                className="w-full rounded-lg px-2 py-1.5 text-center text-xs text-muted-foreground hover:bg-accent disabled:opacity-60"
                disabled={list.isFetchingNextPage}
                onClick={() => void list.fetchNextPage()}
              >
                {list.isFetchingNextPage ? tCommon("table.loading") : tCommon("table.loadMore")}
              </button>
            </li>
          ) : null}
          {!list.isPending && rows.length === 0 ? (
            <li className="px-2 py-1.5 text-sm text-muted-foreground">
              {emptyLabel}
            </li>
          ) : null}
        </ul>
      </PopoverContent>
    </Popover>
  );
}
