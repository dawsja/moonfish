import * as React from "react"
import { cn } from "@/lib/utils"
import { Switch as SwitchPrimitive } from "radix-ui"

function Switch({
  className,
  size = "default",
  children,
  ...props
}: React.ComponentProps<typeof SwitchPrimitive.Root> & {
  size?: "sm" | "default"
}) {
  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      data-size={size}
      className={cn(
        "peer group/switch inline-flex shrink-0 items-center outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50",
        children ? "justify-between gap-3 text-left" : "rounded-full",
        className
      )}
      {...props}
    >
      {children}
      <span
        className={cn(
          "inline-flex shrink-0 items-center rounded-full border border-transparent transition-colors group-data-[state=checked]/switch:bg-primary group-data-[state=unchecked]/switch:bg-input dark:group-data-[state=unchecked]/switch:bg-input/80",
          size === "sm" ? "h-3.5 w-6" : "h-[1.15rem] w-8"
        )}
      >
        <SwitchPrimitive.Thumb
          data-slot="switch-thumb"
          className={cn(
            "pointer-events-none block rounded-full bg-background transition-transform data-[state=checked]:translate-x-[calc(100%-2px)] dark:data-[state=checked]:bg-primary-foreground dark:data-[state=unchecked]:bg-foreground",
            size === "sm" ? "size-3" : "size-4"
          )}
        />
      </span>
    </SwitchPrimitive.Root>
  )
}

export { Switch }
