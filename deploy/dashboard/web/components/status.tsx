import { cn } from "@/lib/utils";

export type Tone = "ok" | "warn" | "bad" | "none" | "info";

const dot: Record<Tone, string> = {
  ok: "bg-ok shadow-[0_0_10px] shadow-ok/70",
  warn: "bg-warn shadow-[0_0_10px] shadow-warn/60",
  bad: "bg-bad shadow-[0_0_10px] shadow-bad/60",
  info: "bg-primary shadow-[0_0_10px] shadow-primary/60",
  none: "bg-muted-foreground/40",
};
export const toneText: Record<Tone, string> = {
  ok: "text-ok",
  warn: "text-warn",
  bad: "text-bad",
  info: "text-primary",
  none: "text-muted-foreground",
};

export function StatusDot({ tone, pulse, className }: { tone: Tone; pulse?: boolean; className?: string }) {
  return (
    <span className={cn("relative inline-flex size-2 shrink-0", className)}>
      {pulse && <span className={cn("absolute inline-flex size-full animate-ping rounded-full opacity-60", dot[tone])} />}
      <span className={cn("relative inline-flex size-2 rounded-full", dot[tone])} />
    </span>
  );
}

/** A small tinted pill: status words in tables. */
export function Pill({ tone, children, className }: { tone: Tone; children: React.ReactNode; className?: string }) {
  const tint: Record<Tone, string> = {
    ok: "bg-ok/12 text-ok ring-ok/25",
    warn: "bg-warn/12 text-warn ring-warn/25",
    bad: "bg-bad/12 text-bad ring-bad/25",
    info: "bg-primary/12 text-primary ring-primary/25",
    none: "bg-muted text-muted-foreground ring-border",
  };
  return (
    <span className={cn("inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset whitespace-nowrap", tint[tone], className)}>
      {children}
    </span>
  );
}

export const jobTone = (s: string): Tone => (s === "done" ? "ok" : s === "failed" ? "bad" : "warn");
