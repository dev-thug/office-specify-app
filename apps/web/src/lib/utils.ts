import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

// globals.css @theme 에 추가한 디자인 토큰 스케일을 tailwind-merge 에 알린다.
// 알리지 않으면 `text-ui` 가 글자 크기가 아니라 글자 색으로 분류돼
// `cn("text-ui", "text-muted-foreground")` 에서 한쪽이 지워진다.
const twMerge = extendTailwindMerge({
  extend: {
    theme: {
      text: ["2xs", "ui"],
      radius: ["xs", "sm", "md", "lg", "xl", "2xl", "3xl", "4xl"],
      ease: ["standard", "emphasized", "exit"],
      blur: ["glass"],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
