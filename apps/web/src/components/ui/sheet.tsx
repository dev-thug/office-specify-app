"use client";

import * as React from "react";
import * as SheetPrimitive from "@radix-ui/react-dialog";
import { AnimatePresence, motion } from "framer-motion";
import { XIcon } from "lucide-react";
import { useTranslations } from "next-intl";

import { cn } from "@/lib/utils";

// Portal orphaning 방지: Dialog와 동일한 forceMount + Framer Motion 패턴.
const SheetOpenContext = React.createContext<boolean | undefined>(undefined);

function Sheet({
  open,
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Root>) {
  return (
    <SheetOpenContext.Provider value={open}>
      <SheetPrimitive.Root data-slot="sheet" open={open} {...props} />
    </SheetOpenContext.Provider>
  );
}

function SheetTrigger({
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Trigger>) {
  return <SheetPrimitive.Trigger data-slot="sheet-trigger" {...props} />;
}

function SheetClose({
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Close>) {
  return <SheetPrimitive.Close data-slot="sheet-close" {...props} />;
}

function SheetPortal({
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Portal>) {
  return <SheetPrimitive.Portal data-slot="sheet-portal" {...props} />;
}

function SheetOverlay({
  className,
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Overlay>) {
  return (
    <SheetPrimitive.Overlay
      data-slot="sheet-overlay"
      className={cn(
        "data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 fixed inset-0 z-50 bg-scrim",
        className
      )}
      {...props}
    />
  );
}

// 방향별 슬라이드 애니메이션 variants
const slideVariants = {
  right: {
    initial: { x: "100%" },
    animate: { x: 0 },
    exit: { x: "100%" },
  },
  left: {
    initial: { x: "-100%" },
    animate: { x: 0 },
    exit: { x: "-100%" },
  },
  top: {
    initial: { y: "-100%" },
    animate: { y: 0 },
    exit: { y: "-100%" },
  },
  bottom: {
    initial: { y: "100%" },
    animate: { y: 0 },
    exit: { y: "100%" },
  },
} as const;

const sheetCloseButtonClass =
  "absolute top-4 right-4 rounded-full opacity-70 transition-[background-color,color,opacity,box-shadow] hover:bg-accent hover:text-accent-foreground hover:opacity-100 outline-none focus-visible:opacity-100 focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none";

function SheetCloseButton() {
  const t = useTranslations("common");
  return (
    <SheetPrimitive.Close className={sheetCloseButtonClass}>
      <XIcon className="size-4" />
      <span className="sr-only">{t("close")}</span>
    </SheetPrimitive.Close>
  );
}

function SheetContent({
  className,
  children,
  side = "right",
  showCloseButton = true,
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Content> & {
  side?: "top" | "right" | "bottom" | "left";
  showCloseButton?: boolean;
}) {
  const open = React.useContext(SheetOpenContext);
  const variant = slideVariants[side];

  // Controlled mode: forceMount + Framer Motion
  if (open !== undefined) {
    return (
      <SheetPrimitive.Portal forceMount data-slot="sheet-portal">
        <AnimatePresence>
          {open && (
            <>
              <SheetPrimitive.Overlay
                forceMount
                data-slot="sheet-overlay"
                asChild
              >
                <motion.div
                  initial={{ opacity: 0 }}
                  animate={{ opacity: 1 }}
                  exit={{ opacity: 0 }}
                  transition={{ duration: 0.15 }}
                  className="fixed inset-0 z-50 bg-scrim"
                />
              </SheetPrimitive.Overlay>
              <SheetPrimitive.Content
                forceMount
                data-slot="sheet-content"
                asChild
                {...props}
              >
                <motion.div
                  initial={variant.initial}
                  animate={variant.animate}
                  exit={variant.exit}
                  transition={{ type: "spring", damping: 30, stiffness: 300 }}
                  className={cn(
                    "bg-popover text-popover-foreground fixed z-50 flex flex-col gap-4 shadow-lg",
                    side === "right" &&
                      "inset-y-0 right-0 h-full w-3/4 border-l sm:max-w-sm",
                    side === "left" &&
                      "inset-y-0 left-0 h-full w-3/4 border-r sm:max-w-sm",
                    side === "top" &&
                      "inset-x-0 top-0 h-auto rounded-b-3xl border-b",
                    side === "bottom" &&
                      "inset-x-0 bottom-0 h-auto rounded-t-3xl border-t",
                    className
                  )}
                >
                  {children}
                  {showCloseButton ? <SheetCloseButton /> : null}
                </motion.div>
              </SheetPrimitive.Content>
            </>
          )}
        </AnimatePresence>
      </SheetPrimitive.Portal>
    );
  }

  return (
    <SheetPortal>
      <SheetOverlay />
      <SheetPrimitive.Content
        data-slot="sheet-content"
        className={cn(
          "bg-popover text-popover-foreground data-[state=open]:animate-in data-[state=closed]:animate-out fixed z-50 flex flex-col gap-4 shadow-lg transition ease-in-out data-[state=closed]:duration-300 data-[state=open]:duration-500",
          side === "right" &&
            "data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right inset-y-0 right-0 h-full w-3/4 border-l sm:max-w-sm",
          side === "left" &&
            "data-[state=closed]:slide-out-to-left data-[state=open]:slide-in-from-left inset-y-0 left-0 h-full w-3/4 border-r sm:max-w-sm",
          side === "top" &&
            "data-[state=closed]:slide-out-to-top data-[state=open]:slide-in-from-top inset-x-0 top-0 h-auto rounded-b-3xl border-b",
          side === "bottom" &&
            "data-[state=closed]:slide-out-to-bottom data-[state=open]:slide-in-from-bottom inset-x-0 bottom-0 h-auto rounded-t-3xl border-t",
          className
        )}
        {...props}
      >
        {children}
        {showCloseButton ? <SheetCloseButton /> : null}
      </SheetPrimitive.Content>
    </SheetPortal>
  );
}

function SheetHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="sheet-header"
      className={cn("flex flex-col gap-1.5 p-4", className)}
      {...props}
    />
  );
}

function SheetFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="sheet-footer"
      className={cn("mt-auto flex flex-col gap-2 p-4", className)}
      {...props}
    />
  );
}

function SheetTitle({
  className,
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Title>) {
  return (
    <SheetPrimitive.Title
      data-slot="sheet-title"
      className={cn("text-foreground font-semibold", className)}
      {...props}
    />
  );
}

function SheetDescription({
  className,
  ...props
}: React.ComponentProps<typeof SheetPrimitive.Description>) {
  return (
    <SheetPrimitive.Description
      data-slot="sheet-description"
      className={cn("text-muted-foreground text-sm", className)}
      {...props}
    />
  );
}

export {
  Sheet,
  SheetClose,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
};
