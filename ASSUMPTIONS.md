# Assumptions and design decisions — Tuvalu Court Register

Everything below is a **design decision made by the developer**, not a fact about
the Tuvalu courts. The project brief (not published in this repository) deliberately leaves these
open; each must be confirmed with the Office of the Judiciary before real use
(§15).

## Platform

1. **Stack override.** Spec §13 suggested Rails + Hotwire + PostgreSQL + Docker
   Compose. The implementation is **Rust (axum) + SQLite + React/Vite/TypeScript**.
   Rationale: one small self-contained binary, minimal memory footprint, no
   database server to operate, single-file encrypted backup — a good fit for a
   small jurisdiction running one modest host. Atomicity guarantees (case
   numbering, hearing booking, finalisation) come from `BEGIN IMMEDIATE`
   single-writer transactions plus database constraints/triggers, not from
   Postgres-specific features. Consequence: single-node deployment only; high
   availability would require re-platforming the storage layer.
2. **Court-operated integrations.** Production uses configurable SMTP and ClamAV;
   no paid API or LLM is required. Demo mail stays local and uses format checks
   only. E-signature validation and OCR have no external integration.

## Calendar, numbering, formats

3. **Fixed timezone.** Court-local time is compiled in as **Pacific/Funafuti =
   UTC+12, no DST** (`src/time.rs`), not configurable. Instants are stored in
   UTC; calendar dates (document date, received date, registration date,
   closure date, due date) are stored separately as court-local `YYYY-MM-DD`.
   The browser timezone never changes a court date.
4. **Case number format** `<SERIES>-<YYYY>-<NNNN>` (e.g. `DEMO-CIV-2026-0001`)
   per registry series and calendar year; uniqueness is enforced by the
   database and gaps are never back-filled. Real courts configure their own
   series/format. Imported legacy numbers are preserved in `legacy_number`, not
   overwritten.
5. **Hearing intervals are `[start, end)`** — 09:00–10:00 does not conflict with
   10:00–11:00. An optional buffer between hearings is the setting
   `hearing_buffer_minutes` (default 0); no buffer is invented.
6. **Intake reference prefix** defaults to `IN`; court name and reference lists
   are settings, editable by `admin.settings`.

## Data model choices

7. **Reference lists are examples.** Case categories, intake channels, origin
   islands, document types, closure bases, hearing types, participant roles,
   dispatch methods, relation kinds and message templates are seeded as
   editable sample data (`src/seed.rs`) — they are **not** official Tuvalu
   classifications.
8. **Persona permission sets** (`src/seed.rs`, also in the README table) are a
   demo design, not an org chart. In production the court grants permissions
   per person; judicial/case-access permissions additionally require the CLI
   `grant` command, not the admin UI.
9. **Intake supplements** are separate intake rows linked to the original
   (`related_intakes`), not file attachments appended in place. A supplement
   cannot be marked ready or registered on its own. Intakes are never deleted;
   a duplicate keeps a link to its source intake.
10. **Closure is a registry state.** `closed` requires a basis, a responsible
    person and resolution of open items (each must be completed, cancelled with
    a reason, or carried forward with a reason). `closed`/`reopened` assert
    nothing about appeals being exhausted or the judgment being final.
11. **Demo-only early outcomes.** Production refuses to record a hearing
    outcome before its start time; demo mode allows it so visitors can finish
    the walkthrough on fictional dates (`src/api/hearings.rs`). For the same
    reason a demo case whose closing evidence (outcome or decision) is dated
    after today closes on exactly that evidence date; production never accepts
    a closing date after today (`src/api/cases.rs`, `validate_closing_basis`).
12. **Dispatch transport depends on mode.** Demo always uses the local mailbox.
    Production sends via configured TLS SMTP and keeps a local sent log; missing
    configuration leaves email queued. Transport failures retry with backoff;
    attachment versions never change after queueing. SMTP acknowledgement loss
    can make delivery ambiguous, so stable Message-ID is helpful but is not a
    guarantee of exactly-once receipt. Human handover and legal service assessment
    remain separate manual records.
13. **No retention/destruction policy** (spec §10). Production never deletes
    cases, intakes or audit rows automatically; the retention schedule is the
    court's decision. Demo sandbox expiry applies to fictional data only.
14. **Optimistic locking & idempotency** are API conventions: editable records
    carry `version`; create-type operations accept `Idempotency-Key`, and a
    replay never repeats the action (a second case number/decision/attempt is
    never produced). The replay response is rebuilt from the current record
    under the caller's current access, not copied from the first response.

## Import / export / backup formats

15. **Legacy case import CSV** columns:
    `number,category,title,registered_date,status,responsible_username,closed_date,closure_basis,parties`.
    Flow is preview → conflict/missing report → idempotent commit. Original
    registration dates are kept; rows with missing data are flagged
    `historical_incomplete` rather than filled with invented facts.
16. **File import package** = a zip with `manifest.csv`
    (`case_number,filename,title,doc_type,visibility,document_date`) plus the
    files. Traversal paths, symlinks, duplicate entries and declared-size
    over-expansion are rejected; the archive cannot write outside its staging
    area. Limits: 20 MB ZIP, 15 MB per file, 40 MB expanded, 500 entries. A
    package with restricted or judicial-note rows belongs to its uploader: only
    they can commit it, and other importers see those rows as restricted until
    the created document is open to them.
17. **Case export (user package)** = a zip containing only the document
    versions the requesting user is permitted to see, plus `manifest.json`
    (`tcr-case-export/1`) with the case's permitted chronology. It is **not** a
    backup and cannot restore restricted data.
18. **Backup format** `.tcrb`: zip manifest + `db.sqlite` snapshot + all stored
    files, encrypted with chunked XChaCha20-Poly1305 (`src/backup.rs`). Restore
    verifies everything before publishing and requires an empty target
    directory.

## To confirm with the court before real use (spec §15)

- Court units, registries, number series and their powers.
- Real case categories and numbering rules.
- Intake/registration procedure: what "ready for registration" means and who
  performs it.
- Who assigns the judge and who finalises decisions.
- Mandatory document types and filing requirements.
- Approved service and copy-delivery methods (whether real e-mail is wanted).
- Access rules, retention and destruction policy.
- How legacy cases should be migrated, and whether an existing system or
  contractor must interoperate.

## Closure evidence and planned work

A closure document/decision/hearing reference and ordered court dates are registry validation
rules for this application, not statements of Tuvalu law. Existing historical/imported closures
are preserved; the migration adds nullable evidence fields without inventing missing evidence.
New closure commands require evidence. Fresh DEMO seeds include a fictional settlement agreement
and reference the finalised order for the other closed case.
“Without a next step” describes current recorded work, rather than legal delay or the absence of
an immediate button to press. Period dates apply to event metrics, while this metric is current.
## Installation and credentials

- Server/maintenance use the same env file. Commands refuse accidental database
  creation and show the installation id and resolved directory. Restore checks
  identity from the existing database inside the authenticated backup and requires
  confirmation before writing an explicit empty destination. The id is preserved.
  Confirmation stages plaintext on destination disk under `.restore-tmp`, with
  normal success/error cleanup; forced termination may leave unpublished staging.
- Production requires clamd or explicit `TCR_AV=off`. Pending/scanner-error files
  are unavailable, including through export, dispatch, mailbox links and imports.
  Built-in format checks and antivirus reduce risk, without guaranteeing safety.
  Imports commit pending versions before scanning outside SQLite writer transactions.
  PDF dictionaries use token parsing and every FlateDecode stream is bounded and inspected.
- New/reset passwords are temporary: all records APIs are blocked until own-password
  change. Own changes require fresh TOTP when enabled, revoke other sessions and
  retain MFA. Technical admins cannot reset protected judicial accounts.
  The password gate starts after completed MFA; login/mode/TOTP remain available
  when a user reloads before entering their code.
- Outbox claims persist an `in_flight` attempt before attachment reads/SMTP, which
  run without SQLite write transactions. Expired claims fail as ambiguous and
  retry with a stable Message-ID. Unreadable attachments require human review and
  do not block later dispatches; SMTP acceptance loss can still mean duplicate receipt.

- Responsible-officer replacement ends the previous active clerk assignment, even if that assignment originally
  came from registration. Another role or general case-view permission can preserve access; registration by
  itself is not a permanent visibility basis. Changing responsibility and registration's named assignee are
  staff-assignment actions with reasons. Judicial officers are assigned only in the judge role.
- Shared-party corrections belong to an actor who can see every linked case/intake and holds `case.edit`,
  `intake.manage` or `case.view_all`. The registry head (Elena in the demo) is the correction owner for contacts
  shared across ordinary cases. This does not bypass restricted-case visibility. A blocked editor sees a neutral
  explanation and no correction form. Audit stores changed field names only.
- Document metadata in nested audit snapshots follows current document access. Inventory redaction changes
  response text only and preserves the append-only audit journal.

Recorded next steps include awaiting a hearing outcome, confirming a draft hearing, pending handover or
service assessment, and deciding the status after reopening. Draft decisions count even when their
private documents are hidden from the viewer. This is a registry workflow metric, not a legal conclusion.
A new closure cannot precede the latest effective status date; imported closures cannot be in the future.

- A re-notification task is fulfilled when an invitation for its replacement hearing and party is queued for e-mail or recorded as manually sent. Completion records the notice ID and actor; it does not establish receipt or legal service. Invitations to adjourned/cancelled hearings remain dispatch history but no longer request handover/service assessment or block closing. Cancellation messages still require follow-up.
- A decision date must be on or after the linked hearing's court-local calendar date, including when the demo permits recording an outcome ahead of time. Draft dates may be blank. Finalisation requires a date; the form defaults to the later of today and the hearing date (or a valid saved date).
