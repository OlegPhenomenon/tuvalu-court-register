# Tuvalu Court Register — Architecture & Contract

Source requirements: the project brief (Russian, not published in this repository; authoritative for behaviour; C01–C18).
Stack override vs spec §13: **Rust (axum) + SQLite + React (Vite, TypeScript)** instead of Rails/Postgres.
Production e-mail uses configured SMTP; antivirus uses configured ClamAV. Signatures and OCR have no external integration. Demo e-mail is always local. UI language: **English**.
All demo names/numbers are fictional and carry `DEMO`.

## 1. Runtime shape

- One binary `tuvalu-court` (crate at repo root, `src/`). Serves `/api/*` JSON and the built React SPA
  (embedded from `web/dist` via `rust-embed`; SPA fallback to `index.html`).
- SQLite via `rusqlite` (bundled), WAL, `foreign_keys=ON`, `busy_timeout=5000`.
  A connection is opened per unit of work (cheap); no pool.
- Background worker = tokio task inside the same process (outbox: queued dispatches → SMTP in production; local mailbox in demo).
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
src/main.rs            CLI: `serve` (default), `create-user`, `grant`, `revoke`, `gen-key`, `backup`, `restore`, `verify-audit`
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
src/mail.rs            TLS SMTP transport, retry backoff and local sent log
src/scan.rs            bounded ClamAV INSTREAM check, fail-closed verdict
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
- Idempotency: `Idempotency-Key` header (client UUID), stored in `operation_keys` in the same transaction as the action.
  Supported on: intake create/request-info/mark-ready/supplement/return/mark-duplicate/link/register; case close/reopen/relation;
  task create/update/complete/cancel/carry-forward; document upload/new-version; party creation, participant addition/ending and
  representation edits; hearing schedule and outcome; decision create/amend/withdraw/finalise; dispatch create/queue/record-sent/confirm.
  Keys bind the actor, operation, target and request hash (multipart: metadata, sanitized filename and file SHA-256). Same key + same
  request replays the stored response (never a second case number / decision / attempt); a changed request returns 409
  `idempotency_mismatch`. Replays recheck current visibility and permissions first. Each opened UI form/dialog keeps one key
  through retries; a new deliberate command uses a new key.
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

Party directory list/search/detail and same-name warnings follow case/intake visibility. A party is visible when linked
as a participant, representative, intake sender, document source or dispatch recipient to a visible record. An unlinked
party is visible only to its creator. Hidden contacts return 404, including when submitted as an existing party id.
Contact writes require visibility of **every** linked case/intake and one of `case.edit`, `intake.manage` or
`case.view_all`. The registry head can correct a contact shared across visible cases without `case.edit`.
If any linked record is hidden, PATCH returns `409 party_shared` with the neutral message "This person is linked to
records you cannot access; ask the registry head"; GET returns `editable:false` and `edit_blocked_reason:"party_shared"`.
The contact editor shows that explanation instead of a form. Hidden record identities are never included.
Equal names remain separate records. Migration 0011 indexes participant, representative, sender, document-source
and dispatch-recipient party links used by the directory policy.

Decision list/detail responses check the exact bound document version. Without document access, `title` and
`document_title` are "Restricted document", `restricted` is true and `document_version_id` remains the reference;
`document_id`, `filename`, `sha256`, `version_no`, `status_reason` and `amendment_basis` are omitted. History/audit
summaries and draft-decision prompts/closure blockers are neutral. Old decision events resolve the document version
bound at that time from draft-edit snapshots, so replacing a restricted draft does not disclose its old metadata.
Case history includes received/completed/supplemented/registered events and information-request events from linked
intakes and their supplements, with original timestamps/authors and no duplicate event ids. Document redaction also
applies to those earlier events. Global audit details undergo recursive document redaction for references in any
nested snapshot, including dispatch before/after items and inventory lines in bodies. Hidden document titles,
filenames and hashes become "Restricted document"; authorised viewers retain the metadata. Stored audit events
and their chain remain unchanged.

Case PATCH changing `responsible_user_id` requires `case.assign_staff`, a non-empty `assignment_reason` and the
same active/eligible-user checks as manual staff assignment. Replacing responsibility ends the previous officer's
active clerk assignment in the same transaction with `end_reason`, `ended_by` and a `case.unassigned` event;
other active roles and general view permissions remain valid. The response includes `residual_access` (remaining
roles), and `case:null` if the assigning actor lost case access. Optional `Idempotency-Key` on case PATCH binds
the complete request and rechecks visibility/permissions before replay. Other case fields still require `case.edit`.
A separate "Change responsible officer" dialog is available with `allowed.assign_staff`; Edit contains no responsible
field. Judicial officers can only receive judge assignments, never staff responsibility.
Intake registration defaults responsibility to the actor. Naming anyone else requires `case.assign_staff` and a
non-empty `assignment_reason` (otherwise 403, with no case/number/assignment created); target eligibility is checked
before the command and its idempotent replay. Registration's officer picker and reason field require assignment
permission, and omit judicial/ineligible accounts. Import preview and
commit use the same assignee eligibility policy and require staff-assignment permission for a named responsible
user; invalid rows are reported individually and skipped. Eligibility changes after preview abort commit with
`409 import_changed` and no partial creation. Imported responsibility creates a clerk assignment, never a judge role.

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

Closing requires exactly one explicit evidence reference: `basis_document_version_id`, `basis_decision_id`, or `basis_hearing_id`.
Without a held hearing, a basis document must belong to the case, be visible to the actor, clean and not a judicial note;
a finalised decision with a visible, clean case document is also allowed. Basis `decided`, or a case with a held hearing,
requires a finalised decision or a held hearing with a recorded outcome. `closed_date` is a court date, no earlier than
registration and the evidence date, and no later than today. Undated documents use their received date, then court-local creation date.
Evidence IDs are kept on the case and in the closure audit event; reopening keeps prior evidence and status history.
`GET /cases/:id/closing-bases` (requires case.close) returns `{items:[{kind:"document"|"decision"|"hearing",id,date,label}]}`
filtered through document policy. The closing dialog selects exact evidence and date and retains validation errors.
Close/reopen and task completion accept `version`; when supplied, stale versions return 409 `version_conflict`. The UI sends it.
Older command clients without a version remain compatible; ordinary PATCH always requires a version.

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
display/CSV columns use labels. `without_next_step` is a current snapshot, independent of report period dates and UI prompts. It counts open visible cases
with no scheduled hearing still ahead/in progress, open task, draft/queued/failed dispatch, or visible draft decision.
The same predicate drives summary hint `plan_next_step`; a future hearing produces `scheduled_hearing`, and queued
messages produce `queued_dispatch`. Counts, drill-down and CSV use the same filtered rows.
Intake references use the numeric maximum of fully parsed suffixes for the configured prefix/year inside BEGIN IMMEDIATE;
formatting uses at least four digits, including 9999 → 10000 → 10001. Existing references stay unchanged.

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
GET/POST /parties  GET/PATCH /parties/:id
PATCH /cases/:id/participants/:pid
GET  /hearings?from=&to=&judge=&room=   POST /cases/:id/hearings   POST /hearings/:id/{confirm,adjourn,cancel,outcome,correct}
GET/POST /cases/:id/tasks   POST /tasks/:id/{complete,cancel,carry-forward}   GET /tasks?mine=1
GET/POST /cases/:id/documents   POST /documents/:id/versions (multipart)   GET /documents/:id
GET  /document-versions/:id/download   PATCH /documents/:id (visibility, version)   POST/DELETE /documents/:id/grants
GET/POST /cases/:id/decisions   POST /decisions/:id/{finalise,amend}
GET/POST /cases/:id/dispatches  GET /dispatches?status=   POST /dispatches/:id/{preview,queue,confirm,assess,cancel}
GET  /mailbox                          demo local e-mail viewer / production SMTP sent log
GET  /reports/summary?from=&to=   GET /reports/:kind/cases (drill-down)   GET /reports/:kind.csv
GET  /search?q=                        cases + document titles, only accessible
POST /import/cases/preview (multipart CSV)   POST /import/:batch/commit   GET /import
POST /cases/:id/export {purpose, version_ids}  -> zip of permitted materials + chronology.json
GET  /audit?case_id=&user_id=
/admin/users, /admin/users/:id/{permissions,deactivate,revoke-sessions,reset-password}, /admin/rooms, /admin/registries,
/admin/ref-items, /admin/templates, /admin/settings
```
`GET /parties/:id` returns `{party, cases, editable, edit_blocked_reason}` (`party_shared` or null); party list items include `version`.
`PATCH /parties/:id` accepts `{version,name,contact_email,contact_phone,address,island,notes?}` and returns the party.
Omitted `notes` is preserved; explicit null clears it. Ordinary optional contact fields accept null to clear.
`PATCH /cases/:id/participants/:pid` accepts `{version,role,representative_party_id,representation_basis,service_contact}`
and returns the updated participation. A representative requires a non-empty basis; null removes the link/basis.
Case participant rows include their own `version` (schema migration 0007); stale contact/participation edits return
`409 version_conflict` with `details.current`. UI contact and participation forms offer an explicit reload after a conflict.
Contact and participation audits record changed field names without copying contact values into the journal.

A case package contains only selected accessible document versions and decision metadata bound to those versions.
An empty selection includes no document or decision metadata/events. Chronology excludes every unselected document,
version or decision event, and dispatch events with unselected attachments. Selected material events have neutral
summaries; amendment/supersession ids of unselected decisions are omitted. The user's ability to read a document
does not automatically select it for a recipient. Default selection remains clean administrative/party material.

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

## Installation, transport and file checks

Server and CLI load `--env-file <path>` or `TCR_ENV_FILE` using literal env-file
values (file values override shell values). Maintenance reads an existing
initialised production database and prints its canonical data directory and
random `installation_id`, created by migration and preserved across backups.
Restore instead reads the initialised DB inside the authenticated backup, prints
the explicit destination and archive installation id, and requires `--yes` or
interactive entry of that id. It never initialises an accidental source `./data`.

`TCR_SMTP_URL=smtp://user:pass@host:587` requires STARTTLS; `smtps://…:465` uses
implicit TLS. `TCR_MAIL_FROM` is required for SMTP delivery; absent configuration
leaves dispatches queued with `Mail transport not configured`, exposed through
Settings and dispatch status. `TCR_SMTP_TIMEOUT_SECS` defaults to 20 (1–120).
The transport uses lettre async SMTP, rustls/ring and bundled webpki roots.
Demo ignores SMTP configuration and warns when SMTP env vars are set. SMTP sends
the exact queued immutable versions, records a new attempt for each failure and
retry, and retains one local mailbox sent log with transport `sent via SMTP`.
Transport failures retry after 60 seconds with exponential backoff capped at
64 minutes; access/attachment failures require human review. Stable Message-ID
is retained across retries. SMTP cannot guarantee exactly-once delivery when
acknowledgement is lost or the process crashes after remote acceptance.
Technical receipt, human handover and legal service assessment remain separate.

`TCR_CLAMD=tcp://host:3310` or `unix:/absolute/socket` uses bounded INSTREAM, with
`TCR_SCAN_TIMEOUT_MS` default 10000 (10–120000). Production refuses startup with
no scanner unless `TCR_AV=off` is explicitly set. Demo always uses format checks
only. Settings and version `scan_note` show when no antivirus is used. HTTP
uploads commit a `pending_scan` row before contacting clamd, then record verdict
and audit atomically; pending bytes cannot be retrieved. Only explicit scanner
OK permits `clean`; infection, errors, timeouts and interrupted scans become
`quarantined`. Pending scans after restart are quarantined. File imports run the
same checks before publication. Download/preview, exports, dispatches, SMTP and
mailbox attachment links require a clean version; storage reads also enforce the
verdict. Full technical backups preserve unavailable versions and their verdicts.
PDF names decode `#xx`; active actions and bounded FlateDecode/object streams
are inspected. PNG validates IHDR, chunk CRCs and IEND; JPEG validates segments
and EOI; DOCX checks bounded XML, macros, ActiveX and OLE content. These checks
reduce risk and do not prove absolute safety.

`GET /auth/me` includes `must_change_password`. While true, middleware denies
every other API route with 403 `password_change_required`, except me, logout,
password change and TOTP verification/enrolment. The UI forces the password form.
`POST /auth/password {current,new,code?}` requires the current password, a
different new password of at least 12 characters, and a fresh TOTP code when
enrolled. It clears the flag, rotates the current session, revokes all other
sessions and preserves TOTP. CLI-created and admin-reset accounts set the flag.
Every production user can change their own password in Settings. Protected-account
reset rules remain enforced server-side.
