"use client";

import { useState } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export function CodeBlock({ code, className }: { code: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className={cn("group relative rounded-lg border bg-muted/40 font-mono text-xs", className)}>
      <pre className="overflow-x-auto p-3 pr-12 leading-relaxed whitespace-pre-wrap break-all">{code}</pre>
      <Button
        size="icon"
        variant="ghost"
        className="absolute top-1.5 right-1.5 size-7 opacity-60 group-hover:opacity-100"
        onClick={() => {
          navigator.clipboard?.writeText(code);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
        aria-label="Copy"
      >
        {copied ? <CheckIcon className="size-3.5 text-ok" /> : <CopyIcon className="size-3.5" />}
      </Button>
    </div>
  );
}
