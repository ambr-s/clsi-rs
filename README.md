# clsi-rs

A Rust CLSI for Overleaf, deployable to Cloudflare Containers.

Replaces the upstream Overleaf [CLSI service](https://github.com/overleaf/overleaf/tree/main/services/clsi)
with a single-binary HTTP server that speaks the same protocol, runs
`latexmk` in a sandboxed subprocess, and uploads compile outputs to
Cloudflare R2 so the browser fetches PDFs directly from CF's edge.

## Why

The upstream CLSI is a Node.js service that has to be co-located with
Overleaf web. clsi-rs is a single ~5 MB binary that:

- Runs on Cloudflare Containers (or Fly, or anywhere with a Linux+texlive
  base image)
- Sandboxes `latexmk` properly: strips secrets from the child env, forces
  `-no-shell-escape` on all engines, refuses user-supplied `.latexmkrc`
- Uploads outputs to R2 directly so the browser bypasses web for PDF fetches
- Uses per-user routing via a Cloudflare Worker + Durable Object so each
  user's compiles land on a sticky warm container (aux files persist between
  compiles)

## Layout

```
src/             Rust HTTP server (axum + tokio).
worker/          Cloudflare Worker (TypeScript) that fronts the
                 container DO. Adds auth at the edge, routes output
                 GETs directly to R2.
scripts/         Helpers — bench.sh hits the deployed worker and
                 prints cold/warm timings.
Dockerfile       Multi-stage: rust:slim build stage, debian:bookworm
                 -slim runtime with apt-installed texlive packages.
wrangler.jsonc   CF Worker + Container config (1 vCPU / 4 GiB / 8 GB
                 disk per instance, max 15 instances).
podman-as-docker.sh  Wrangler-compatible shim around podman for builds
                 on hosts without Docker.
```

## Run locally

```bash
podman build -t clsi-rs:dev .
podman run --rm -p 3013:3013 \
  -e CLSI_SHARED_AUTH=$(openssl rand -hex 24) \
  -e R2_ENDPOINT=https://<account>.r2.cloudflarestorage.com \
  -e R2_BUCKET=overleaf-clsi-output \
  -e R2_ACCESS_KEY_ID=... \
  -e R2_SECRET_ACCESS_KEY=... \
  clsi-rs:dev
```

## Deploy to Cloudflare

```bash
npx wrangler secret put CLSI_SHARED_AUTH
npx wrangler secret put R2_ENDPOINT
npx wrangler secret put R2_BUCKET
npx wrangler secret put R2_ACCESS_KEY_ID
npx wrangler secret put R2_SECRET_ACCESS_KEY
WRANGLER_DOCKER_BIN=$PWD/podman-as-docker.sh npx wrangler deploy
```

## Protocol

clsi-rs implements the subset of upstream CLSI's HTTP API that
Overleaf web actually uses:

| Method | Path | Notes |
|--------|------|-------|
| `POST` | `/project/:pid[/user/:uid]/compile` | Main compile entrypoint. |
| `DELETE` | `/project/:pid[/user/:uid]` | Clear aux files + R2 outputs for scope. |
| `GET` | `/project/:pid/status` | Per-project liveness. |
| `GET` | `/health_check`, `/status` | Server liveness (no auth). |

Request and response shapes mirror upstream's `RequestParser.js` and
`CompileController.js` so a vanilla Overleaf web hitting this service
sees a drop-in CLSI.

## Auth

Single shared bearer token (`CLSI_SHARED_AUTH`). Set on the Worker as a
secret; the Worker validates and rejects unauth'd traffic at the edge
(without waking the container). The container also re-validates.

## Used by

A fork of Overleaf CE that hits this service for compiles:
[github.com/ambr-s/overleaf](https://github.com/ambr-s/overleaf) (branch
`ambersys/clsi-rs-r2`). That fork patches Overleaf web to presign R2
URLs for history blobs (sent to clsi-rs in the compile request) and to
302 the browser to R2 for output PDFs.
