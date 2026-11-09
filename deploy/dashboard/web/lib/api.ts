"use client";

import useSWR from "swr";

// What dashboard.py serves at /api/state (the nameserver's views plus docker, jobs and the settlement loop).
export type Heartbeat = { age_s: number; classes: string[]; load_mbps: number; version: string };
export type DedanNode = {
  label: string;
  name: string;
  ipv4: string | null;
  ipv6: string | null;
  location: { lat: number; lon: number } | null;
  class: string;
  stake: number;
  active: boolean;
  up?: boolean;
  verified?: boolean;
  heartbeat?: Heartbeat | null;
  owner: string;
  payout: string;
  port: number;
  signers: string[];
  version: string;
};
export type Voter = { attester: string; rtt_ms: number; ts: number };
export type CellNode = { label: string; median_ms: number; voters: Voter[] };
export type Cell = {
  cell: string;
  name: string;
  centre: { lat: number; lon: number };
  boundary: [number, number][];
  humans: number;
  members: string[];
  nodes: CellNode[];
};
export type Rank = { cell: string; rings: number; humans: number; nodes: [string, number][] };
export type Rejection = { reason: string | Record<string, unknown>; auditor: string; node?: string; cell: string; ts: number };
export type Human = {
  city: string;
  kind: "city" | "client";
  attester: number;
  cell: string;
  location: string;
  ip: string;
  verified_at_start: boolean;
};
export type Container = { id: string; name: string; status: string };
export type Job = { kind: string; label: string; status: "running" | "done" | "failed"; log: string };
export type Period = {
  period: number;
  start: number;
  end: number;
  registry_block: number;
  total_cum_payout: string | number;
  total_cum_bill: string | number;
  published: boolean;
  tx?: string | null;
  payout_root?: string;
  bill_root?: string;
  inputs_hash?: string;
};
type Failed = { error: string };
export type State = {
  dozzle: string;
  containers: Container[];
  jobs: Record<string, Job>;
  cities: Human[];
  settlement: { period_secs: number; periods: Period[] } | null;
  nodes: { block: number; nodes: DedanNode[] } | Failed;
  cells: { block: number; cells: Cell[] } | Failed;
  map: { ranks: Rank[]; rejected: Rejection[]; attestations: number; reveals: number } | Failed;
  assignment: { sets: Record<string, string[]>; suspended: string[]; sticky_on_period: number | null } | Failed;
};

const stamped = async (url: string) => ({ ...(await json(url)), fetched_at: Date.now() });

const json = async (url: string) => {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return r.json();
};

/** One poll every 5 s, shared by every component that asks (SWR dedupes by key). */
export function useDevnet() {
  const { data, error, isLoading } = useSWR<State & { fetched_at: number }>("/api/state", stamped, { refreshInterval: 5000, keepPreviousData: true });
  const ok = <T,>(v: T | Failed | undefined): T | undefined => (v && !("error" in (v as object)) ? (v as T) : undefined);
  const nodes = ok(data?.nodes);
  const cells = ok(data?.cells);
  const map = ok(data?.map);
  const assignment = ok(data?.assignment);
  return {
    state: data,
    fetchedAt: data?.fetched_at,
    error,
    isLoading,
    block: nodes?.block,
    nodes: nodes?.nodes ?? [],
    cells: cells?.cells ?? [],
    ranks: map?.ranks ?? [],
    rejected: map?.rejected ?? [],
    attestations: map?.attestations ?? 0,
    reveals: map?.reveals ?? 0,
    sets: assignment?.sets ?? {},
    humans: data?.cities ?? [],
    containers: data?.containers ?? [],
    jobs: data?.jobs ?? {},
    settlement: data?.settlement ?? null,
    dozzle: data?.dozzle ?? "http://localhost:9999",
  };
}

export function useJSON<T>(url: string | null, refreshInterval = 0) {
  return useSWR<T>(url, json, { refreshInterval });
}

export async function post<T = Record<string, unknown>>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || `${r.status} ${r.statusText}`);
  return j as T;
}

/** Terminal colour codes (tracing output) removed for display. */
export const stripAnsi = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, "");
export const usdc = (x: string | number) => Number(x) / 1e6;
export const fmtUsdc = (x: string | number, digits = 2) =>
  usdc(x).toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits });
export const short = (a: string, head = 6, tail = 4) => (a.length > head + tail + 1 ? `${a.slice(0, head)}…${a.slice(-tail)}` : a);
export const time = (ts: number) => new Date(ts * 1000).toLocaleTimeString();
export const containerName = (label: string) => `dedan-devnet-${label}-1`;
export const shortContainer = (name: string) => name.replace(/^dedan-devnet-/, "").replace(/-1$/, "");
/** The cell's best median, or null before any vote. */
export const bestMs = (c: Cell) => (c.nodes.length ? c.nodes[0].median_ms : null);
export const latencyTone = (ms: number | null): "ok" | "warn" | "bad" | "none" => (ms === null ? "none" : ms <= 30 ? "ok" : ms <= 120 ? "warn" : "bad");
