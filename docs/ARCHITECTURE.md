# Tuvalu Court Register — Architecture & Contract

Source requirements: the project brief (Russian, not published in this repository; authoritative for behaviour; C01–C18).
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
409 `stale_review` (decision review no longer matches; details: current `version`, `document_version_id`) /
`version_conflict` (details: `current` object) / `hearing_conflict` (details: conflicting hearings) / `invalid_transition` / `open_items` / `duplicate`,
413 `too_large`, 415 `unsupported_type`.

### Mutations
- CSRF: cookies are `SameSite=Strict; HttpOnly`; all non-GET requests must send header `X-TCR: 1`, else 403 `csrf`.
- Optimistic locking: editable records have `version INTEGER`; PATCH bodies include `version`; mismatch → 409 `version_conflict` with current record.
- Idempotency: `Idempotency-Key` header (client UUID) on register/close/finalise/schedule, decision create/amend/withdraw,
  dispatch create/queue/record-sent/confirm, and hearing outcome; stored in `operation_keys`;
  same actor, key and command body replay the stored response; a different body returns 409 `idempotency_mismatch`.
  Authorization is checked again before replay. UI forms keep one key throughout retries.
- Every response for private data: `Cache-Control: no-store, private`.

## 3. Time
- Instants stored as UTC text `YYYY-MM-DDTHH:MM:SSZ` (lexicographically comparable).
- Calendar dates (document date, received date, registration date, closure date, due date) stored as `YYYY-MM-DD` court-local, **separately**.
- API takes hearing times as court-local `starts_local`/`ends_local` (`YYYY-MM-DDTHH:MM`) and returns both UTC and local.
  Browser timezone never influences court dates. Interval is `[start, end)`: 09:00–10:00 does not clash with 10:00–11:00.
  Calendar ranges select overlaps (`starts_at < range_end AND ends_at > range_start`); `from` and `to` are inclusive
  court dates, converted to UTC midnight bounds. Day/week/month and print show each overlapping day, clipped to
  00:00–24:00 with “continued”/“continues” markers. An end exactly at midnight does not occupy the next day.
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
An assignment requires an active user with a case, hearing, task, document, decision, dispatch or case-export permission.
System-only administrators cannot be assigned, including through the responsible-officer field. `/api/ref` staff entries include `assignable: bool`.

### Document access (policy::can_view_document) — on top of case access
| visibility | who |
|---|---|
| `administrative` | anyone with case access |
| `party_material` | anyone with case access |
| `restricted` | uploader, or active `document_grants` row |
| `judicial_note` | author only, or explicit active share (`document_grants`) — never via admin role |
Opening/downloading a restricted file or judicial-note version writes `document.viewed_restricted` after its bytes are loaded.
Document metadata/detail and list reads do not record views. Case history, exports and `/audit` omit document events
outside the viewer's document access and `case.related` events whose counterpart case is hidden.
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
- Dispatch (notice or copy package): `draft → queued → sent|failed|superseded`;
  `draft|failed → superseded` when bound material becomes obsolete; superseded is terminal and requires a fresh dispatch; `failed → queued` (retry = new attempt row). Technical delivery receipt,
  human confirmation and legal service assessment are **separate** records.
- Task: `open → done|cancelled(reason)|carried_forward(reason)`.

Closing a case with open tasks, draft decisions, draft/queued/failed dispatches, draft/scheduled hearings or sent dispatches
without a `human_handover` confirmation → 409 `open_items`, with `details.items: [{kind,id,label,status}]`.
Sent dispatches have kind `unconfirmed_dispatch`; close accepts optional
`acknowledge: [{kind:"unconfirmed_dispatch",id,reason}]`. Each must be confirmed or explicitly acknowledged with a non-empty reason.
Other blockers must be resolved (complete, cancel with reason, or carry tasks forward with reason).
Acknowledgements are stored in `case.closed` audit details under `acknowledge` and appended to `closure_note` as
`Left unconfirmed: <label> — <reason>`. All changes and audit records are atomic; close is idempotent.

Registering or linking an intake cancels its and its supplements' draft/queued information requests, recording
`status_reason: "Not sent: the filing was registered"` and an audit event per request. Already sent/failed requests remain.
The response includes `cancelled_requests: [{id,kind,recipient_name,status,status_reason}]`; obsolete `missing_items` are cleared.
Intake detail has `next_actions: [{code,message,link}]`, including a draft request's review/send link `/dispatch?dispatch=<id>`.
Request-info still returns `{ok,dispatch_id}`.

Adjournment requires a changed start (`400`, "Choose a new date or time"). Adjourn and `outcome.next_hearing`
accept `override_reason`, with the same conflict checks, permission and audit rules as creation.
Draft hearing PATCH accepts `room_id: null` and `judge_user_id: null`; task PATCH accepts `assignee_user_id: null`;
draft decision PATCH accepts `decision_date: null`. Null clears these values; omission preserves them.

Judicial review commands bind to the exact record shown to the reviewer:
- `POST /decisions/:id/finalise` requires `{version, document_version_id, decision_date, signed_file_uploaded?}`.
  A draft row or bound file mismatch returns 409 `stale_review` with `{version, document_version_id}`; no state change.
  The UI reloads the draft, identifies the file/revision and requires a fresh review before retrying.
- `POST /hearings/:id/confirm` requires `{version, override_reason?}`. A changed draft slot returns
  409 `version_conflict` with `{current}` before booking. The UI reloads time/room/judge for review.
- Hearing outcome's early-recording exception applies only to DEMO. Production refuses future hearings.

Dispatch material and currency:
- Create accepts `kind: notice|copies|decision_copy|working_document`; the last three are stored as a copy package.
  `decision_copy` requires every selected `version_ids` entry to be the exact bound version of a currently
  finalised decision. `working_document` explicitly sends ordinary material (including drafts). Legacy `copies`
  derives each item's kind from its current decision binding. Items return `material_kind` and `decision_id`.
  The existing `dispatch.manage` decision-copy permission and document visibility/clean-scan rules apply.
- Working material always carries “DRAFT / working material” in subject/body and mailbox attachments;
  finalised-only packages carry “Copy of finalised decision”. Custom edits retain these labels.
- Hearing invitations store `hearing_id`, `hearing_version`, `hearing_starts_at` at preparation and verify them
  when previewing, then bind the review to those current values. Hearing edits, confirmation, adjournment,
  cancellation, outcomes and corrections atomically supersede unsent invitations, with an audit event.
  Sent notices remain unchanged. Adjournment creates tasks; no replacement notice is automatically queued.
- The `hearing_cancellation` template derives `notice_purpose: cancellation`; other hearing notices derive
  `invitation`. Cancellation notices and document copies are independent of hearing cancellation.
- Queue uses a JSON command body (normally `{}`) in its idempotency fingerprint. Queue/manual handover/retry
  validate currency; the worker checks again within the delivery transaction and marks stale invitations or
  decision copies `superseded` before any delivery attempt/mailbox entry. The UI displays
  “Superseded — hearing changed; prepare a new notice” (or the corresponding decision-copy reason).

## 6. Next-action messages
`GET /api/cases/:id` returns `next_actions: [{code, message, link}]` computed server-side, e.g.
"Confirm delivery of the hearing notice to Maria Tanaka", "Record the outcome of the hearing on 19 Nov 2026", "Finalise or withdraw the draft decision".
Work queue aggregates these for the current user. Assignment links include `?tab=summary&action=assign-judge`;
copy links include `?tab=dispatch&action=copies&decision=<id>&party=<id>` to select the decision's exact bound version and recipient.
Dispatch and hearing actions include `dispatch=<id>` or `hearing=<id>` on their respective case tabs.
Delivery messages identify the recipient and hearing's court-local date (or subject when no hearing is bound);
decision-copy messages include the decision title and recipient.

Templates render `{hearing_local}` and `{previous_local}` as English court-local text, e.g.
"Tuesday 17 November 2026 at 09:00". Seeded sign-offs use `{court}` once. Dispatch audit summaries identify
kind, recipient and document count instead of repeating the subject's court name; mailbox delivery adds the address.
Mailbox attachments include `document_version_id`. Missing/unknown versions are redacted instead of causing a 404.
Report drill-down rows keep codes and add `category_label`, `status_label`, `closure_basis_label` (when applicable);
display/CSV columns use labels. `without_next_step` counts open visible cases with an empty computed `next_actions` list.

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
