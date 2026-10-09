"use client"

import {
  CircleCheckIcon,
  InfoIcon,
  Loader2Icon,
  OctagonXIcon,
  TriangleAlertIcon,
  XIcon,
} from "lucide-react"
import { useTheme } from "next-themes"
import { Toaster as Sonner, type ToasterProps } from "sonner"

const Toaster = ({ ...props }: ToasterProps) => {
  const { theme = "system" } = useTheme()

  return (
    <Sonner
      theme={theme as ToasterProps["theme"]}
      className="toaster group"
      icons={{
        success: <CircleCheckIcon className="size-4" />,
        info: <InfoIcon className="size-4" />,
        warning: <TriangleAlertIcon className="size-4" />,
        error: <OctagonXIcon className="size-4" />,
        loading: <Loader2Icon className="size-4 animate-spin" />,
        close: <XIcon className="size-3.5" />,
      }}
      closeButton
      position="bottom-right"
      gap={8}
      style={
        {
          "--normal-bg": "var(--popover)",
          "--normal-text": "var(--popover-foreground)",
          "--normal-border": "var(--border)",
          // 떠 있는 표면 반경(16px)과 같다
          "--border-radius": "var(--radius-2xl)",
          // overlay elevation 과 같은 그림자 색 토큰
          "--shadow":
            "inset 0 1px 0 var(--shadow-highlight), 0 1px 2px var(--shadow-ambient), 0 8px 24px -8px var(--shadow-key)",
        } as React.CSSProperties
      }
      {...props}
    />
  )
}

export { Toaster }
