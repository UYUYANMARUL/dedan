"use client";

import { useEffect, useRef } from "react";
import { ThemeProvider } from "next-themes";
import { toast } from "sonner";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useDevnet, type Job } from "@/lib/api";

/** A toast when a job the dashboard started finishes (jobs present at first load stay quiet). */
function JobToasts() {
  const { jobs, state } = useDevnet();
  const seen = useRef<Record<string, Job["status"]> | null>(null);
  useEffect(() => {
    if (!state) return;
    const prev = seen.current;
    seen.current = Object.fromEntries(Object.entries(jobs).map(([id, j]) => [id, j.status]));
    if (!prev) return;
    for (const [id, j] of Object.entries(jobs)) {
      if (prev[id] === j.status || j.status === "running") continue;
      const what = `${j.kind} ${j.label}`;
      if (j.status === "done") toast.success(`${what} finished`, { description: `job ${id}` });
      else toast.error(`${what} failed`, { description: `job ${id}: see Operations for the log` });
    }
  }, [jobs, state]);
  return null;
}

export function Providers({ children }: { children: React.ReactNode }) {
  return (
    <ThemeProvider attribute="class" defaultTheme="dark" enableSystem disableTransitionOnChange>
      <TooltipProvider delayDuration={150}>
        {children}
        <JobToasts />
        <Toaster richColors position="bottom-right" />
      </TooltipProvider>
    </ThemeProvider>
  );
}
