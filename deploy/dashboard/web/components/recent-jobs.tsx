"use client";

import { ChevronRightIcon } from "lucide-react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { stripAnsi, useDevnet } from "@/lib/api";
import { Pill, StatusDot, jobTone } from "@/components/status";

/** The dashboard's jobs (newest first), optionally only some kinds; running ones open. */
export function RecentJobs({ kinds, limit = 50, empty = "Nothing run yet." }: { kinds?: string[]; limit?: number; empty?: string }) {
  const { jobs } = useDevnet();
  const list = Object.entries(jobs)
    .filter(([, j]) => !kinds || kinds.some((k) => j.kind === k || j.kind.startsWith(k + "-")))
    .reverse()
    .slice(0, limit);
  if (!list.length) return <p className="text-sm text-muted-foreground">{empty}</p>;
  return (
    <div className="divide-y rounded-lg border">
      {list.map(([id, j], i) => (
        <Collapsible key={id} defaultOpen={j.status === "running" || i === 0}>
          <CollapsibleTrigger className="group flex w-full items-center gap-3 px-3 py-2.5 text-left text-sm hover:bg-muted/40">
            <ChevronRightIcon className="size-4 text-muted-foreground transition-transform group-data-[state=open]:rotate-90" />
            <StatusDot tone={jobTone(j.status)} pulse={j.status === "running"} />
            <span className="font-medium">{j.kind}</span>
            <span className="truncate font-mono text-xs text-muted-foreground">{j.label}</span>
            <span className="ml-auto flex items-center gap-2">
              <Pill tone={jobTone(j.status)}>{j.status}</Pill>
              <span className="font-mono text-xs text-muted-foreground">{id}</span>
            </span>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <pre className="mx-3 mb-3 max-h-80 overflow-auto rounded-md border bg-black/40 p-3 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-all">
              {stripAnsi(j.log)}
            </pre>
          </CollapsibleContent>
        </Collapsible>
      ))}
    </div>
  );
}
