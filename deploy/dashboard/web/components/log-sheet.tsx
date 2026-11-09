"use client";

import { useEffect, useRef, useState } from "react";
import { ScrollTextIcon, SquareTerminalIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { containerName, stripAnsi, useDevnet } from "@/lib/api";
import useSWR from "swr";

function Tail({ name }: { name: string }) {
  const { data } = useSWR<string>(`/api/logs/${containerName(name)}?tail=300`, (u: string) => fetch(u).then((r) => r.text()), {
    refreshInterval: 3000,
  });
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" }); // returns a Promise in newer browsers: not a cleanup
  }, [data]);
  return (
    <div className="min-h-0 flex-1 overflow-auto rounded-lg border bg-black/40 p-3 font-mono text-[11px] leading-relaxed text-foreground/90">
      <pre className="whitespace-pre-wrap break-all">{data === undefined ? "loading…" : stripAnsi(data) || "(no output yet)"}</pre>
      <div ref={end} />
    </div>
  );
}

/** "Logs" button: the container's last 300 lines, refreshed every 3 s while open. */
export function LogSheet({ name }: { name: string }) {
  const [open, setOpen] = useState(false);
  return (
    <Sheet open={open} onOpenChange={setOpen}>
      <SheetTrigger asChild>
        <Button variant="ghost" size="sm" className="h-7 gap-1.5 px-2 text-xs">
          <ScrollTextIcon className="size-3.5" /> Logs
        </Button>
      </SheetTrigger>
      <SheetContent className="flex w-full flex-col gap-3 sm:max-w-3xl">
        <SheetHeader>
          <SheetTitle className="font-mono">{containerName(name)}</SheetTitle>
          <SheetDescription>docker logs --tail 300, live</SheetDescription>
        </SheetHeader>
        <div className="flex min-h-0 flex-1 flex-col px-4 pb-4">{open && <Tail name={name} />}</div>
      </SheetContent>
    </Sheet>
  );
}

/** The container's page in Dozzle, where its shell is one keystroke away (DOZZLE_ENABLE_SHELL in compose). */
export function TerminalLink({ name }: { name: string }) {
  const { containers, dozzle } = useDevnet();
  const c = containers.find((x) => x.name === containerName(name));
  if (!c) return <span className="px-2 text-xs text-muted-foreground">no container</span>;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button variant="ghost" size="sm" className="h-7 gap-1.5 px-2 text-xs" asChild>
          <a href={`${dozzle}/container/${c.id.slice(0, 12)}`} target="_blank" rel="noreferrer">
            <SquareTerminalIcon className="size-3.5" /> Terminal
          </a>
        </Button>
      </TooltipTrigger>
      <TooltipContent>Opens {containerName(name)} in Dozzle: press ⌘⇧E (Ctrl+Shift+E) or its Shell button for a shell</TooltipContent>
    </Tooltip>
  );
}

