# Security model — Tuvalu Court Register

This document describes how the application protects court records and where its
boundaries are. The single access policy (`src/policy.rs`) is shared by screens,
file downloads, search, report counts, exports and the background worker — there
is no UI-only filtering.

## Access model

### Permissions

Authorisation is permission-based (rows in `user_permissions`), not role-based:

`intake.manage`, `case.register`, `case.view_all`, `case.view_restricted`,
`case.edit`, `case.assign_staff`, `case.assign_judge`, `case.close`,
`case.reopen`, `hearing.schedule`, `hearing.override_conflict`,
`hearing.record_outcome`, `hearing.admin_correct`, `task.manage`,
`document.manage`, `document.grant_restricted`, `decision.draft`,
`decision.finalise`, `dispatch.manage`, `dispatch.assess_service`,
`report.view`, `audit.view`, `import.run`, `export.case`, `admin.users`,
`admin.settings`.

An `admin.users` holder can grant only a restricted subset from the UI
(`ADMIN_GRANTABLE` in `src/policy.rs`). Judicial and case-access powers —
`case.view_all`, `case.view_restricted`, `case.assign_judge`, `decision.*`,
`hearing.record_outcome`, `hearing.admin_correct`, `hearing.override_conflict`,
`document.grant_restricted`, `dispatch.assess_service`, `audit.view`,
`import.run`, `admin.users` — can only be granted from the command line
(`tuvalu-court grant`), so a technical administrator cannot escalate himself
into the judiciary.

### Case and document visibility

A case is visible if the user has an **active assignment** on it, or holds
`case.view_all` and the case is not restricted, or holds `case.view_restricted`.
This applies identically to lists, search suggestions, report counts, exports,
file downloads and queued dispatches. Ending an assignment ends access
immediately — stored URLs re-check permissions on every request.

Document visibility layers on top of case access:

| Visibility | Who can see it |
|---|---|
| `administrative`, `party_material` | anyone with access to the case |
| `restricted` | uploader, or a user with an active `document_grants` row |
| `judicial_note` | author only, or explicit active grant — never via the admin role |

Unlinked intake documents are visible to `intake.manage` holders. Viewing a
restricted document or a judicial note writes a `document.viewed_restricted`
audit event.

### 404, not 403

An object the actor may not see returns `404 not_found` — the system never
reveals that a hidden case, document or intake exists. `403 forbidden` is
returned only when the object is visible but the action needs a permission the
actor lacks. Knowing a case number grants no access.

Party directory access and existing-party links are scoped to visible cases/intakes (including
representatives, senders and document sources). A creator can see an unlinked contact. Editing a
contact shared with a hidden case returns neutral `409 party_shared`; known hidden ids return 404.
Contact changes audit field names without copying contact values. Same-name warnings are scoped too.

Restricted decision document references are redacted in list/detail, next actions, closure blockers,
history and global audit, including old versions replaced during drafting. A participant export
filters metadata, links and chronology by its selected versions as well as the exporter's permissions.
Intake events become part of linked case history without rewriting the append-only journal.

Changing responsibility through case PATCH and importing a named responsible user require staff
assignment permission and an active user with case-work permissions. Imports cannot grant technical
administrators case access. Party/participation command retries recheck access before replay.
Judicial finalisation checks both the reviewed decision row version and exact document version
inside the write transaction. Draft hearing confirmation likewise checks its reviewed version.
Unsent invitations are superseded atomically when a hearing changes. Before delivery, the worker
rechecks the hearing binding and finalised decision-copy binding as well as current access.
Stale material is retained as terminal `superseded` history and never delivered. Working documents,
including drafts, carry an explicit DRAFT / working material label in the message and mailbox.

## Sessions, cookies, CSRF

- Session tokens are 256-bit random values; only their SHA-256 hash is stored.
- Cookies are `HttpOnly; SameSite=Strict; Path=/`; `Secure` is added when
  `TCR_COOKIE_SECURE=true` (always set it behind HTTPS).
- Sessions expire after `TCR_SESSION_HOURS` (default 12), are revoked on
  password change and user deactivation, and can be revoked by an administrator
  (`revoke-sessions`). The session token is rotated after the second factor is
  completed (session-fixation defence).
- CSRF: every non-GET request must send the header `X-TCR: 1`, and when a
  browser sends `Origin` its host must equal `Host`. Either failure → `403
  csrf`. Combined with `SameSite=Strict` this blocks cross-site form posts.
- Private responses carry `Cache-Control: no-store, private`.

## Passwords and second factor

- Passwords are hashed with **Argon2id** (argon2 crate defaults); minimum length
  12 characters.
- Second factor is **TOTP** (RFC 6238, SHA-1, 6 digits, 30 s step; the current
  step ±1 is accepted). Enrolled at first login; a used time step is recorded
  (`totp_last_step`) so the **same code cannot be replayed**.
- Brute-force lockout: 5 failed attempts per username or 20 per IP within
  15 minutes → `429 locked`.
- Password verification for unknown usernames runs against a dummy hash to
  equalise timing.

## File handling

- Allowed types: **PDF, DOCX, JPEG, PNG** — detected from magic bytes, never
  from the client-supplied name or content type; the extension must match the
  detected type.
- HTML, SVG and executables are refused outright. PDFs containing JavaScript,
  launch actions or embedded files, and DOCX files with macros or implausible
  expansion, are stored but **quarantined** (`scan_status=quarantined`) and
  never served.
- Filenames are sanitised to a safe basename; storage keys are
  `<2 hex>/<sha256>-<random>` and strictly validated before touching the
  filesystem.
- Files live in a private directory and are never served statically; every
  download re-checks permissions and the SHA-256 integrity of the stored bytes.
- Downloads are `Content-Disposition: attachment`, `X-Content-Type-Options:
  nosniff`, `Cache-Control: no-store`, and a sandboxing
  `Content-Security-Policy: sandbox; default-src 'none'` so nothing executes in
  the application's origin.

## Audit trail

- `audit_events` is **append-only**: UPDATE/DELETE are blocked by database
  triggers. Every state change writes its audit event in the same transaction
  as the change itself.
- Each event's SHA-256 hash chains on the previous event. `tuvalu-court
  verify-audit` recomputes the chain and reports the first broken event.
- The chain is **tamper-evident, not tamper-proof**: anyone with write access
  to the SQLite file could rewrite it wholesale. It reliably detects
  modification, it does not prevent it — keep the data directory and backups
  access-controlled.

## Demo isolation

- Each visitor's sandbox is a separate SQLite file + files directory under a
  random 128-bit id stored in an `HttpOnly` cookie; the id is validated before
  it ever touches a path. One visitor cannot see, reset or evict another's
  sandbox.
- Sandboxes expire after `TCR_SANDBOX_TTL_HOURS` (72 h default) and are capped
  by `TCR_MAX_SANDBOXES` (200); at capacity only sandboxes idle >2 h are
  evicted, otherwise new visitors get `503`. Per-sandbox upload quota:
  `TCR_SANDBOX_QUOTA_MB` (25 MB).
- Demo personas have unusable random password hashes; persona login exists only
  in demo mode.

## Backups

- `tuvalu-court backup` produces a `.tcrb` file: a zip of `manifest.json`
  (table counts, schema version, audit head hash, per-file SHA-256), the
  database snapshot (`VACUUM INTO`, so it is consistent) and every stored file —
  encrypted as 1 MiB **XChaCha20-Poly1305** chunks with a random nonce prefix,
  the chunk index and finality bound as additional authenticated data.
- The key is a 32-byte hex file created by `gen-key`. **Keep it off the server
  and out of the repository.**
- `restore` only accepts a new or empty directory, verifies authentication,
  manifest, row counts, foreign keys, file checksums and the audit head, and
  publishes files only after every check passes.

## Known limitations

- The app reads `X-Forwarded-For` / `X-Real-IP` for rate limiting and audit IP
  records. Deploy only behind a proxy that **overwrites** these headers (the
  shipped nginx config does); direct exposure lets clients spoof them.
- Format checks and antivirus are risk reduction, not proof of absolute safety. Demo and explicit `TCR_AV=off` installations use format checks only.
- No OCR and no full-text search over file contents (search covers case fields
  and document titles).
- Demo dispatches stay in the local mailbox. Production SMTP uses TLS, records transport failures and retries, and never marks unconfigured delivery as sent. SMTP acknowledgement loss can produce ambiguous delivery; it cannot guarantee universal exactly-once receipt.
- No qualified electronic signature: uploading a signed file records the file;
  the system does not assert signature validity or legal effect.
- Single-node SQLite deployment: no replication or HA; rely on the encrypted
  backup for disaster recovery.
- Browser timezone changes cannot alter court dates (fixed UTC+12), but the
  display of times assumes the court operates in Pacific/Funafuti.

## Reporting a vulnerability

Please do not open a public issue. Open a **private GitHub security advisory**
for the repository (Security tab → Advisories → "Report a vulnerability").
Include steps to reproduce and the affected version/commit. If the repository
is not on GitHub, contact the maintainer directly through the channel the
software was delivered by.

## Workflow retries and closure evidence

Command keys belong to the authenticated user and bind the operation, target and request hash.
The action, audit event and stored response commit atomically. A replay rechecks current object
visibility and permissions; state changes such as closing or registering cannot duplicate a committed upload.
Multipart requests bind metadata and file SHA-256. Closing evidence must be visible, belong to the
case and use a clean version; judicial notes are excluded. The evidence picker uses the same policy.
Case cards and closure audit details redact evidence IDs when the viewer cannot see their document.
## Installation checks added after audit

New and reset accounts must change their temporary password after TOTP.
`/auth/me` exposes the flag; server middleware blocks all other API use except
me/logout/password/TOTP enrolment and verification with `password_change_required`.
Own-password changes require the current password and fresh TOTP (if enrolled),
preserve TOTP, rotate the current session and revoke other sessions. Technical
admins cannot reset judges or protected accounts.

Production must configure private clamd (`TCR_CLAMD`, TCP or Unix INSTREAM) or
explicitly set `TCR_AV=off`. Configured scanner errors and timeouts fail closed.
Uploads are pending until a verdict; only explicit OK allows clean. Startup
quarantines interrupted pending checks. Download, inline preview, export, dispatch,
SMTP attachments and mailbox links cannot retrieve pending/quarantined bytes.
Imports use the same checks; full backups retain verdicts. Built-in checks include
escaped PDF names and bounded FlateDecode streams, PNG CRC/structure, JPEG
segments/EOI and DOCX macros/ActiveX/OLE. Neither these checks nor antivirus prove
absolute file safety; unsupported PDF stream encodings are quarantined when they
prevent object-stream inspection.

SMTP requires STARTTLS or implicit TLS with rustls/ring and bundled webpki roots;
credentials never appear in Settings. Demo ignores SMTP configuration. Production
without complete SMTP configuration keeps email queued. Successful deliveries
retain the exact queued version inventory in a local log marked sent via SMTP;
this does not assert legally sufficient service.

Maintenance commands load the server env file, print data directory and installation
id, and refuse an absent/uninitialised source database. Restore reads the existing
DB inside an authenticated backup and requires explicit confirmation before writing
a new/empty destination. Installation ids survive restore. Keep env files, data,
backups and keys private; see `docs/OPERATIONS.md` for absolute-path commands.
