"use client";

import { useMemo } from "react";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  COMMON_TIMEZONES,
  detectTimezone,
} from "@/features/meetings/datetime";

interface TimezoneSelectProps {
  value: string;
  onValueChange: (tz: string) => void;
  /** aria-label / placeholder 텍스트는 호출자가 i18n 으로 넘긴다. */
  ariaLabel: string;
  /** 감지된 브라우저 시간대 등 목록에 없어도 맨 위에 고정할 값. */
  pinned?: string[];
  disabled?: boolean;
  className?: string;
  id?: string;
}

/**
 * IANA 시간대 선택 — 대표 목록 + 감지/지정된 pinned 값을 맨 위에 보여 준다.
 * 각 항목은 IANA 시간대 이름으로 표시한다.
 */
export function TimezoneSelect({
  value,
  onValueChange,
  ariaLabel,
  pinned = [],
  disabled,
  className,
  id,
}: TimezoneSelectProps) {
  const zones = useMemo(() => {
    const seen = new Set<string>();
    const list: string[] = [];
    for (const z of [...pinned, value, detectTimezone(), ...COMMON_TIMEZONES]) {
      if (!seen.has(z)) {
        seen.add(z);
        list.push(z);
      }
    }
    return list;
  }, [pinned, value]);

  return (
    <Select value={value} onValueChange={onValueChange} disabled={disabled}>
      <SelectTrigger
        id={id}
        size="sm"
        aria-label={ariaLabel}
        className={className}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {zones.map((zone) => (
          <SelectItem key={zone} value={zone}>
            {zone}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
