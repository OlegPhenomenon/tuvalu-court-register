# Tuvalu Court Register — Architecture & Contract

Source requirements: `docs/spec/SPEC_RU.txt` (Russian, authoritative for behaviour; C01–C18).
Stack override vs spec §13: **Rust (axum) + SQLite + React (Vite, TypeScript)** instead of Rails/Postgres.
Everything outside the court (e-mail, signatures, OCR) is local/mocked. UI language: **English**.
All demo names/numbers are fictional and carry `DEMO`.

## 1. Runtime shape

- One binary `tuvalu-court` (crate at repo root, `src/`). Serves `/api/*` JSON and the built React SPA
  (embedded from `web/dist` via `rust-embed`; SPA fallback to `index.html`).
- SQLite via `rusqlite` (bundled), WAL, `foreign_keys=ON`, `busy_timeout=5000`.
  A connection is opened per unit of work (cheap); no pool.
- Background worker = tokio task inside the same process (outbox: queued dispatches → local mailbox).
- Files: private directory `<data>/files/<aa>/<sha256-hex>-<random>`; never served statically.
- Low footprint is a requirement: no heavy deps (no ORM, no chrono-tz, no OpenSSL). Release profile: `lto="thin"`, `strip=true`, `opt-level="s"`.

### Modes
- `TCR_MODE=demo` (public showcase): every visitor gets an **isolated sandbox** = its own SQLite file +
  files dir, copied from a seeded template. Persona switch (Olga/Elena/Viktor/Sergei/Pavel) works only in demo mode.
  One visitor can never reset or see another visitor's sandbox. Sandboxes expire after `TCR_SANDBOX_TTL_HOURS` (default 72)
  and are capped at `TCR_MAX_SANDBOXES` (default 200, oldest evicted). Demo upload quota per sandbox `TCR_SANDBOX_QUOTA_MB` (default 25).
- `TCR_MODE=production`: single DB `<data>/court.sqlite`, password + TOTP login, no persona switch, no TTL.

### Config (env)
`TCR_MODE`, `TCR_DATA_DIR` (default `./data`), `TCR_BIND` (default `127.0.0.1:8088`), `TCR_COOKIE_SECURE` (bool),
`TCR_SANDBOX_TTL_HOURS`, `TCR_MAX_SANDBOXES`, `TCR_SANDBOX_QUOTA_MB`, `TCR_UPLOAD_MAX_MB` (default 15).

## 2. Code layout (backend)

```
src/main.rs            CLI: `serve` (default), `migrate`, `create-admin`, `backup`, `restore`, `seed-demo`
src/config.rs          Config from env
src/db.rs              Db handle {db_path, files_dir}; read()/write() helpers; migrations runner
src/migrations/*.sql   Schema (0001_init.sql is the full schema)
src/error.rs           AppError -> JSON {error:{code,message,details}}
src/time.rs            UTC timestamps, court-local (Pacific/Funafuti = fixed UTC+12, no DST) conversions
src/auth.rs            sessions, password (argon2), TOTP, lockout, extractors `Ctx` / `Auth`
src/policy.rs          permissions + case/document access (single policy for screens, files, search, counts, exports, worker)
src/audit.rs           append-only audit (hash chained)
src/sandbox.rs         demo sandbox manager
src/storage.rs         file store: validate type by magic bytes, sha256, quarantine
src/worker.rs          outbox processing
src/seed.rs            demo seed data (the DEMO_DATA)
src/api/mod.rs         router composition; each module exposes `pub fn routes() -> Router<AppState>`
src/api/{auth,intake,cases,parties,hearings,tasks,documents,decisions,dispatch,mailbox,reports,search,
         import,export,admin,audit_log,queue,demo}.rs
tests/*.rs             integration tests through the HTTP router (tower `oneshot`), temp data dir
```

### DB helper contract
```rust
db.read(move |conn: &rusqlite::Connection| -> Result<T, AppError> {..}).await
db.write(move |tx: &rusqlite::Transaction| -> Result<T, AppError> {..}).await  // BEGIN IMMEDIATE; commit on Ok
```
Both run on `spawn_blocking`. **Every state change + its audit event + its outbox row happen in one `write`.**
Concurrency guarantees (case numbering, hearing booking, finalisation) rely on `BEGIN IMMEDIATE` (single writer)
**plus** DB constraints/triggers (UNIQUE numbering, hearing-overlap trigger), so a bug in app code still cannot double-book.

### Request context
`Ctx` extractor = `{ db: Db, actor: Actor, ip }` (401 if no session / MFA not completed; in demo mode 401 code `no_sandbox`
if the sandbox cookie is missing/expired). `Actor { user_id, username, display_name, perms: BTreeSet<String>, is_judge }`.

### Errors
JSON `{"error":{"code":"snake_case","message":"Human readable","details":{}}}`.
400 `validation`, 401 `unauthenticated`/`no_sandbox`/`mfa_required`, 403 `forbidden` (missing permission on an object the actor can see),
404 `not_found` (also returned when the actor may not see the object — never leak existence),
409 `version_conflict` (details: `current` object) / `hearing_conflict` (details: conflicting hearings) / `invalid_transition` / `open_items` / `duplicate`,
413 `too_large`, 415 `unsupported_type`.

### Mutations
- CSRF: cookies are `SameSite=Strict; HttpOnly`; all non-GET requests must send header `X-TCR: 1`, else 403 `csrf`.
- Optimistic locking: editable records have `version INTEGER`; PATCH bodies include `version`; mismatch → 409 `version_conflict` with current record.
- Idempotency: `Idempotency-Key` header (client UUID) on register/close/finalise/queue-dispatch/schedule; stored in `operation_keys`;
  replay returns stored result, never a second case number / decision / attempt.
- Every response for private data: `Cache-Control: no-store, private`.

## 3. Time
- Instants stored as UTC text `YYYY-MM-DDTHH:MM:SSZ` (lexicographically comparable).
- Calendar dates (document date, received date, registration date, closure date, due date) stored as `YYYY-MM-DD` court-local, **separately**.
- API takes hearing times as court-local `starts_local`/`ends_local` (`YYYY-MM-DDTHH:MM`) and returns both UTC and local.
  Browser timezone never influences court dates. Interval is `[start, end)`: 09:00–10:00 does not clash with 10:00–11:00.
  Optional buffer between hearings = setting `hearing_buffer_minutes` (default 0).

## 4. Permissions (strings in `user_permissions`)
| permission | meaning |
|---|---|
| `intake.manage` | receive intake, request info, mark duplicate, link, return/redirect |
| `case.register` | register case from intake / create case |
| `case.view_all` | see all non-restricted cases |
| `case.view_restricted` | see restricted cases without assignment |
| `case.edit` | edit case card, participants |
| `case.assign_staff` | assign/unassign non-judge staff |
| `case.assign_judge` | assign/unassign judge (separate from registry head role) |
| `case.close` / `case.reopen` | close with basis / reopen with reason |
| `hearing.schedule` | create/confirm/adjourn/cancel hearings |
| `hearing.override_conflict` | confirm a conflicting hearing with reason (audited) |
| `hearing.record_outcome` | record held/attendance/result |
| `hearing.admin_correct` | correct an erroneous held hearing (with reason) |
| `task.manage` | create/complete/cancel tasks |
| `document.manage` | upload documents/versions, set visibility |
| `document.grant_restricted` | grant/revoke access to restricted documents |
| `decision.draft` / `decision.finalise` | draft decisions / finalise & amend |
| `dispatch.manage` | prepare/review/queue dispatches, record delivery |
| `dispatch.assess_service` | record legal assessment of service |
| `report.view` | reports & CSV |
| `audit.view` | view audit log |
| `import.run` | legacy import |
| `export.case` | case package export |
| `admin.users` | manage users, permissions, deactivate, revoke sessions |
| `admin.settings` | rooms, registries, reference lists, templates, settings |

Personas (demo): **Olga** (clerk): intake.manage, case.register, case.edit, hearing.schedule, task.manage, document.manage, dispatch.manage, case.close, report.view, export.case.
**Elena** (registry head): case.view_all, case.assign_staff, case.assign_judge, case.reopen, case.close, report.view, audit.view, import.run, export.case, document.grant_restricted, hearing.override_conflict.
**Viktor** (judge): decision.draft, decision.finalise, hearing.record_outcome, hearing.admin_correct, task.manage, dispatch.assess_service; sees cases where assigned.
**Sergei** (service officer): dispatch.manage, hearing.schedule, task.manage.
**Pavel** (tech admin): admin.users, admin.settings — **no case access, no judicial notes, cannot delete audit.**

### Case access (policy::can_view_case)
Active assignment (`case_assignments.end_at IS NULL`) **or** (`case.view_all` and case not restricted) **or** `case.view_restricted`.
Applies to lists, search suggestions, report counts (count only visible cases), exports, file downloads, the worker.
Ending an assignment immediately ends access (including old file URLs — downloads re-check on every request).

### Document access (policy::can_view_document) — on top of case access
| visibility | who |
|---|---|
| `administrative` | anyone with case access |
| `party_material` | anyone with case access |
| `restricted` | uploader, or active `document_grants` row |
| `judicial_note` | author only, or explicit active share (`document_grants`) — never via admin role |
Viewing a restricted document or judicial note writes an audit event `document.viewed_restricted`.
Intake documents (no case yet): `intake.manage` holders.

## 5. State machines (server-enforced; invalid → 409 `invalid_transition`)
- Intake: `received → needs_information → received|ready_for_registration`; `received|ready_for_registration → linked_to_case` (register or link);
  any non-linked → `returned_or_redirected` (reason required); any non-linked → `duplicate` (link to original required). Intakes are never deleted.
- Case: `registered → active → on_hold ↔ active`; `registered|active|on_hold → closed` (basis + responsible + no unexplained open items);
  `closed → reopened` (reason) → `active|on_hold|closed`. Never back to draft, never deleted.
- Hearing: `draft → scheduled → held|adjourned|cancelled`; `draft → cancelled`. Adjourn creates a **new linked hearing** (old stays `adjourned`
  with reason + authoriser; new gets `previous_hearing_id`), frees the slot, generates "notify again" tasks per participant. `held` cannot be adjourned;
  correction via `hearing.admin_correct` with reason. Held never closes the case.
- Decision: `draft → finalised → superseded` (by a finalised amendment, linked); `draft → withdrawn` (reason). Finalised file can never be replaced in place.
- Dispatch (notice or copy package): `draft → queued → sent|failed`; `failed → queued` (retry = new attempt row). Technical delivery receipt,
  human confirmation and legal service assessment are **separate** records.
- Task: `open → done|cancelled(reason)|carried_forward(reason)`.

Closing a case with open tasks/draft or queued dispatches/scheduled hearings → 409 `open_items` listing them; client resolves each
(complete, cancel with reason, or carry forward with reason) then closes.

## 6. Next-action messages
`GET /api/cases/:id` returns `next_actions: [{code, message, link}]` computed server-side, e.g.
"Confirm delivery of the hearing notice to Maria Tanaka", "Record the outcome of the hearing on 19 Nov 2026", "Finalise or withdraw the draft decision".
Work queue aggregates these for the current user.

## 7. API surface (all JSON, prefix `/api`)
```
GET  /health
POST /demo/start                       -> creates sandbox, sets cookie
GET  /demo/personas                    POST /demo/login {persona}      (demo mode only)
POST /demo/reset                       (resets ONLY caller's own sandbox)
POST /auth/login {username,password}   POST /auth/totp {code}   POST /auth/logout   GET /auth/me
GET  /queue                            work queue for current user
GET/POST /intakes   GET/PATCH /intakes/:id   POST /intakes/:id/{request-info,mark-ready,mark-duplicate,return,link,register,supplement}
GET  /cases?q=&status=&category=&party=&responsible=&from=&to=   GET/PATCH /cases/:id
POST /cases/:id/{status,close,reopen,relations,participants,assignments}   POST /cases/:id/assignments/:aid/end
GET/POST /parties  GET /parties/:id
GET  /hearings?from=&to=&judge=&room=   POST /cases/:id/hearings   POST /hearings/:id/{confirm,adjourn,cancel,outcome,correct}
GET/POST /cases/:id/tasks   POST /tasks/:id/{complete,cancel,carry-forward}   GET /tasks?mine=1
GET/POST /cases/:id/documents   POST /documents/:id/versions (multipart)   GET /documents/:id
GET  /document-versions/:id/download   PATCH /documents/:id (visibility, version)   POST/DELETE /documents/:id/grants
GET/POST /cases/:id/decisions   POST /decisions/:id/{finalise,amend}
GET/POST /cases/:id/dispatches  GET /dispatches?status=   POST /dispatches/:id/{preview,queue,confirm,assess,cancel}
GET  /mailbox                          local e-mail viewer (demo/production: nothing leaves the server)
GET  /reports/summary?from=&to=   GET /reports/:kind/cases (drill-down)   GET /reports/:kind.csv
GET  /search?q=                        cases + document titles, only accessible
POST /import/cases/preview (multipart CSV)   POST /import/:batch/commit   GET /import
POST /cases/:id/export {purpose, version_ids}  -> zip of permitted materials + chronology.json
GET  /audit?case_id=&user_id=
/admin/users, /admin/users/:id/{permissions,deactivate,revoke-sessions,reset-password}, /admin/rooms, /admin/registries,
/admin/ref-items, /admin/templates, /admin/settings
```
Technical full backup/restore is **CLI only** (`tuvalu-court backup --out f.tcrb --key keyfile`), encrypted
(ChaCha20-Poly1305, key file kept outside repo), manifest with counts + sha256 of every file, verified on restore into an empty data dir.

## 8. Frontend (`web/`)
Vite + React + TypeScript + react-router. No UI kit; one CSS file, accessible (labels, focus, keyboard), responsive, print CSS for reports.
`src/api.ts` fetch wrapper (adds `X-TCR: 1`, `Idempotency-Key`, parses error JSON). Unsaved form text kept in component state and shown
with "Not saved — retry"; **no** localStorage/IndexedDB of case data. Screens: Work queue, Incoming, Cases, Case workspace (tabs: Summary,
Participants, Documents, Hearings, Decisions, Dispatch, Tasks, History), Calendar (day/week/month), Documents, Decisions, Dispatch,
Mailbox, Reports, Import, Audit, Settings, Demo landing + persona switcher banner ("DEMO — fictional data").

## 9. Tests
Integration tests in `tests/` hit the router with a temp data dir: every state transition, every access denial (case, restricted doc,
judicial note, ended assignment + old URL, admin without case access, search/report counts), concurrency (parallel registration →
distinct numbers; parallel conflicting hearing confirmation → one wins), idempotent replay, backup → restore round trip.
