import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import * as React from "react";

import { cn } from "@/lib/utils";

// solid variant 는 `bg-{role}` + `text-{role}-foreground`, subtle variant 는 옅은 틴트 위에
// `text-{role}-on-subtle`. 모양은 pill(rounded-full).
const badgeVariants = cva(
  "inline-flex items-center justify-center rounded-full border px-2.5 py-0.5 text-xs font-medium leading-5 w-fit whitespace-nowrap shrink-0 [&>svg]:size-3 gap-1 [&>svg]:pointer-events-none outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px] aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive transition-[color,box-shadow] overflow-hidden",
  {
    variants: {
      variant: {
        default:
          "border-transparent bg-primary text-primary-foreground [a&]:hover:bg-primary/90",
        secondary:
          "border-transparent bg-secondary text-secondary-foreground [a&]:hover:bg-secondary/90",
        destructive:
          "border-transparent bg-destructive text-destructive-foreground [a&]:hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:focus-visible:ring-destructive/40",
        outline:
          "text-foreground [a&]:hover:bg-accent [a&]:hover:text-accent-foreground",
        brand: "border-transparent bg-brand text-brand-foreground [a&]:hover:bg-brand/90",
        success: "border-transparent bg-success text-success-foreground [a&]:hover:bg-success/90",
        info: "border-transparent bg-info text-info-foreground [a&]:hover:bg-info/90",
        teal: "border-transparent bg-teal text-teal-foreground [a&]:hover:bg-teal/90",
        amber: "border-transparent bg-amber text-amber-foreground [a&]:hover:bg-amber/90",
        rose: "border-transparent bg-rose text-rose-foreground [a&]:hover:bg-rose/90",
        "brand-subtle": "border-brand/20 bg-brand/10 text-brand-on-subtle",
        "success-subtle": "border-success/20 bg-success/10 text-success-on-subtle",
        "info-subtle": "border-info/20 bg-info/10 text-info-on-subtle",
        "teal-subtle": "border-teal/20 bg-teal/10 text-teal-on-subtle",
        "amber-subtle": "border-amber/25 bg-amber/10 text-amber-on-subtle",
        "rose-subtle": "border-rose/20 bg-rose/10 text-rose-on-subtle",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
);

function Badge({
  className,
  variant,
  asChild = false,
  ...props
}: React.ComponentProps<"span"> &
  VariantProps<typeof badgeVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "span";

  return (
    <Comp
      data-slot="badge"
      className={cn(badgeVariants({ variant }), className)}
      {...props}
    />
  );
}

export { Badge, badgeVariants };
