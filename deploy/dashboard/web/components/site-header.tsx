"use client";

import { usePathname } from "next/navigation";
import { MoonIcon, SunIcon } from "lucide-react";
import { useTheme } from "next-themes";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { SidebarTrigger } from "@/components/ui/sidebar";
import { useDevnet } from "@/lib/api";

const titles: Record<string, string> = {
  "/": "Overview",
  "/network/": "Latency map",
  "/nodes/": "Nodes",
  "/humans/": "Clients & attestation",
  "/world-id/": "World ID",
  "/settlement/": "Settlement",
  "/ops/": "Operations",
};

export function SiteHeader() {
  const path = usePathname();
  const { resolvedTheme, setTheme } = useTheme();
  const { fetchedAt } = useDevnet();
  const updated = fetchedAt ? new Date(fetchedAt).toLocaleTimeString() : "";
  return (
    <header className="sticky top-0 z-20 flex h-14 shrink-0 items-center gap-2 rounded-t-xl border-b bg-background/70 px-4 backdrop-blur-md">
      <SidebarTrigger className="-ml-1" />
      <Separator orientation="vertical" className="mx-1 data-[orientation=vertical]:h-4" />
      <span className="text-sm text-muted-foreground">Dedan devnet</span>
      <span className="text-sm text-muted-foreground">/</span>
      <span className="text-sm font-medium">{titles[path] ?? titles[path + "/"] ?? ""}</span>
      <div className="ml-auto flex items-center gap-3">
        {updated && <span className="hidden font-mono text-xs text-muted-foreground sm:inline">updated {updated}</span>}
        <Button
          variant="ghost"
          size="icon"
          className="size-8"
          onClick={() => setTheme(resolvedTheme === "dark" ? "light" : "dark")}
          aria-label="Toggle theme"
        >
          <SunIcon className="size-4 dark:hidden" />
          <MoonIcon className="hidden size-4 dark:block" />
        </Button>
      </div>
    </header>
  );
}
