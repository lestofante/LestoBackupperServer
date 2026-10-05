# Web Interface

Minimal browsing and sharing UI. See `00-core.md` and `02-server.md`.

## Scope

- Browse albums and previews
- Manage album membership and roles

**No upload, no delete.** Those stay device-only, so the browser never triggers an
ingest pipeline.

## One service

The web UI, the device sync API, and server-to-server mirror reconciliation are **one
HTTP service** with different roles per authenticated client. One auth system, one port.

## Rendering

Gallery views read `preview` blobs, so a grid is a single query with no file opens.

Clicking a photo stays at preview quality. Full content reaches a device only via an
explicit pull through a real client — serving originals from the browser would create a
second egress path that bypasses the sync model.
