// CF Worker → clsi-rs Container.
//
// Per-user routing: each user_id maps to its own Durable Object → its own
// container. Different users compile in parallel, each user's repeat compiles
// land on the same warm container so aux files stay cached.
//
// Auth: clsi-rs handles bearer auth itself. The Worker does a no-header reject
// pre-DO so bot traffic on the *.workers.dev hostname can't spin containers.

import { Container, getContainer } from "@cloudflare/containers";

// Containers in WEUR — closest to the R2 bucket location and most users in EU.
// locationHint is only honored at DO creation; established DOs stay put.
const LOCATION_HINT = "weur" as DurableObjectLocationHint;

// CE-image used ~25s; our Rust binary boots in <1s. Generous timeout covers
// the slowest hop (image pull from CF's registry on cold colos).
const PORT_WAIT_MS = 60_000;

export class ClsiContainer extends Container<Env> {
  defaultPort = 3013;
  requiredPorts = [3013];
  // Keeps the container alive across an active editing session. Cost: 8 GiB
  // memory provisioned for 2min after every burst.
  sleepAfter = "5m";
  enableInternet = true;

  override async fetch(request: Request): Promise<Response> {
    // start() with explicit envVars, then wait for the port. Two-step because
    // envVars belong on start(), not startAndWaitForPorts(), and we want them
    // present from the very first boot.
    await this.start({
      envVars: {
        CLSI_SHARED_AUTH: this.env.CLSI_SHARED_AUTH,
        R2_ENDPOINT: this.env.R2_ENDPOINT,
        R2_BUCKET: this.env.R2_BUCKET,
        R2_ACCESS_KEY_ID: this.env.R2_ACCESS_KEY_ID,
        R2_SECRET_ACCESS_KEY: this.env.R2_SECRET_ACCESS_KEY,
      },
    });
    await this.startAndWaitForPorts(this.requiredPorts, {
      portReadyTimeoutMS: PORT_WAIT_MS,
      abort: request.signal,
    });
    return this.containerFetch(request, this.defaultPort);
  }
}

interface Env {
  CLSI: DurableObjectNamespace<ClsiContainer>;
  CLSI_SHARED_AUTH: string;
  R2_ENDPOINT: string;
  R2_BUCKET: string;
  R2_ACCESS_KEY_ID: string;
  R2_SECRET_ACCESS_KEY: string;
  OUTPUTS: R2Bucket;
}

// Match CLSI output paths:
//   /project/:pid[/user/:uid]/build/:bid/output/:file
// On match, returns the key into the OUTPUTS bucket. Otherwise returns null
// and the request falls through to container routing.
function outputKey(pathname: string): string | null {
  const m = pathname.match(
    /^\/project\/([^/]+)(?:\/user\/([^/]+))?\/build\/([^/]+)\/output\/(.+)$/,
  );
  if (!m) return null;
  const [, pid, uid, bid, file] = m;
  const scope = uid ? `${pid}-${uid}` : pid;
  return `project/${scope}/build/${bid}/output/${file}`;
}

const CONTENT_TYPES: Record<string, string> = {
  pdf: "application/pdf",
  log: "text/plain; charset=utf-8",
  fls: "text/plain; charset=utf-8",
  stdout: "text/plain; charset=utf-8",
  stderr: "text/plain; charset=utf-8",
  gz: "application/gzip",
};

function contentTypeFor(file: string): string {
  const dot = file.lastIndexOf(".");
  if (dot < 0) return "application/octet-stream";
  return CONTENT_TYPES[file.slice(dot + 1)] || "application/octet-stream";
}

async function serveFromR2(
  bucket: R2Bucket,
  key: string,
  request: Request,
): Promise<Response> {
  // Honor Range requests so PDF.js can stream-load the PDF.
  const range = request.headers.get("range");
  const opts: R2GetOptions = {};
  if (range) {
    const m = range.match(/^bytes=(\d+)-(\d*)$/);
    if (m) {
      const offset = parseInt(m[1], 10);
      const length = m[2] ? parseInt(m[2], 10) - offset + 1 : undefined;
      opts.range = length !== undefined ? { offset, length } : { offset };
    }
  }
  const obj = await bucket.get(key, opts);
  if (!obj) return new Response("not found", { status: 404 });

  const headers = new Headers();
  obj.writeHttpMetadata(headers);
  headers.set("etag", obj.httpEtag);
  // Force a sensible content-type — R2's saved type for binary uploads is
  // sometimes generic.
  const file = key.split("/").pop()!;
  headers.set("content-type", contentTypeFor(file));
  headers.set("accept-ranges", "bytes");
  if (obj.range) {
    const r = obj.range as { offset: number; length: number };
    const end = r.offset + r.length - 1;
    headers.set("content-range", `bytes ${r.offset}-${end}/${obj.size}`);
    return new Response(obj.body, { status: 206, headers });
  }
  return new Response(obj.body, { headers });
}

// CLSI URL shapes we route on:
//   /project/:pid/user/:uid/...   → routing key = uid
//   /project/:pid/...             → routing key = pid (no user in URL, e.g.
//                                   anonymous /compile or /status calls)
//   anything else                 → "default" (health probes, etc.)
function routingKey(pathname: string): string {
  const userMatch = pathname.match(/^\/project\/[^/]+\/user\/([^/]+)/);
  if (userMatch) return `u:${userMatch[1]}`;
  const projectMatch = pathname.match(/^\/project\/([^/]+)/);
  if (projectMatch) return `p:${projectMatch[1]}`;
  return "default";
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === "/cf-health") {
      return new Response("ok\n");
    }

    // Edge-level no-auth reject so unauth'd traffic never wakes a container
    // or hits R2.
    if (!request.headers.has("authorization")) {
      return new Response("unauthorized", { status: 401 });
    }
    // Constant-time check at the edge — the container would also check, but
    // we want output GETs (which skip the container) gated identically.
    const expected = `Bearer ${env.CLSI_SHARED_AUTH}`;
    const got = request.headers.get("authorization") || "";
    if (!env.CLSI_SHARED_AUTH || !timingSafeEqual(got, expected)) {
      return new Response("unauthorized", { status: 401 });
    }

    // Output files come straight from R2 — skip the container hop entirely.
    if (request.method === "GET" || request.method === "HEAD") {
      const key = outputKey(url.pathname);
      if (key) return serveFromR2(env.OUTPUTS, key, request);
    }

    const key = routingKey(url.pathname);
    return getContainer(env.CLSI, key, { locationHint: LOCATION_HINT }).fetch(
      request,
    );
  },
} satisfies ExportedHandler<Env>;

function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}
