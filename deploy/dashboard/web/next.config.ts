import type { NextConfig } from "next";

// Production: a static export (out/) that dashboard.py serves next to its API on :9090.
// Development (`npm run dev`): the API calls go to the running dashboard container.
const api = process.env.DASHBOARD_API ?? "http://127.0.0.1:9090";
const dev = process.env.NODE_ENV !== "production";

const nextConfig: NextConfig = {
  output: dev ? undefined : "export",
  trailingSlash: true,
  images: { unoptimized: true },
  ...(dev && {
    rewrites: async () =>
      ["/api/:path*", "/clients", "/nodes"].map((source) => ({ source, destination: `${api}${source}` })),
  }),
};

export default nextConfig;
