import type { LucideIcon } from "lucide-react";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";

export function StatCard({
  label,
  value,
  hint,
  icon: Icon,
  accent = "from-primary/15",
  children,
}: {
  label: string;
  value: React.ReactNode;
  hint?: React.ReactNode;
  icon: LucideIcon;
  accent?: string;
  children?: React.ReactNode;
}) {
  return (
    <Card className="relative gap-0 overflow-hidden py-0">
      <div className={cn("pointer-events-none absolute inset-0 bg-gradient-to-br to-transparent to-60%", accent)} />
      <CardContent className="relative space-y-2 p-5">
        <div className="flex items-center justify-between text-sm text-muted-foreground">
          {label}
          <Icon className="size-4" />
        </div>
        <div className="text-3xl font-semibold tracking-tight tabular-nums">{value}</div>
        {hint && <div className="text-xs text-muted-foreground">{hint}</div>}
        {children}
      </CardContent>
    </Card>
  );
}
