"use client";

import { MotionConfig } from "framer-motion";
import type { ReactNode } from "react";

/**
 * prefers-reduced-motion 을 Framer Motion 트리 전체에 적용한다 —
 * "user" 는 시스템 설정 시 transform 애니메이션(이동·확대)을 생략한다.
 */
export function MotionProvider({ children }: { children: ReactNode }) {
  return <MotionConfig reducedMotion="user">{children}</MotionConfig>;
}
