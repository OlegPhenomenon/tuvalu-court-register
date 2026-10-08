# Test results

Recorded 8 October 2026 on `main` at commit `dfd093d` (R04 fix after the review of `136d796`, plus the last
specification gaps found in a C01–C18 walkthrough). The requirement-by-requirement checklist is
[`ACCEPTANCE.md`](ACCEPTANCE.md).

Tool versions: rustc/cargo 1.98.1, clippy 0.1.98, Node 23.10.0, npm 11.19.0, TypeScript 5.9.3, Vite 8.3.3,
Docker 29.4.3 (linux/arm64), Python 3.10.11. Host: macOS 26 (arm64).

## Build, lint, types

| Command | Result |
|---|---|
| `cargo build` | finished, 0 warnings |
| `cargo clippy --all-targets` | 0 warnings, 0 errors (`clippy.toml`: argument limit 8) |
| `cd web && npm run typecheck` | `tsc --noEmit`, no errors |
| `cd web && npm run build` | `dist/assets/index-*.css 19.8 kB`, `index-*.js 502 kB (gzip 143 kB)`, built |

## Automated tests (`cargo test`)

Per test binary, as printed by cargo:

| Test binary | Result |
|---|---|
| `src/lib.rs` (unit) | ok. 9 passed; 0 failed |
| `src/main.rs` | ok. 0 passed; 0 failed |
| `tests/acceptance_t37.rs` | ok. 1 passed; 0 failed |
| `tests/admin_seed.rs` | ok. 7 passed; 0 failed |
| `tests/audit_access.rs` | ok. 29 passed; 0 failed |
| `tests/audit_install.rs` | ok. 46 passed; 0 failed |
| `tests/audit_judicial.rs` | ok. 16 passed; 0 failed |
| `tests/audit_workflow.rs` | ok. 25 passed; 0 failed |
| `tests/backup.rs` | ok. 1 passed; 0 failed |
| `tests/decisions_dispatch.rs` | ok. 15 passed; 0 failed |
| `tests/documents.rs` | ok. 15 passed; 0 failed |
| `tests/e2e_backend.rs` | ok. 8 passed; 0 failed |
| `tests/hearings_tasks.rs` | ok. 25 passed; 0 failed |
| `tests/import_export.rs` | ok. 13 passed; 0 failed |
| `tests/import_visibility_regression.rs` | ok. 3 passed; 0 failed |
| `tests/intake_cases.rs` | ok. 11 passed; 0 failed |
| `tests/reaudit_regressions.rs` | ok. 11 passed; 0 failed |
| `tests/reports_search.rs` | ok. 6 passed; 0 failed |
| Doc-tests | ok. 0 passed; 0 failed |

241 tests passed, 0 failed. (One test in `audit_install.rs` re-runs the test binary as a child process to check
command-line behaviour; that child prints an extra `1 passed; 45 filtered out` line, which is not a separate test.)
The suite was run three times in a row without a failure (`cargo test --locked`). An earlier run in parallel with
the Docker clean-install build failed once in `r3_restore_revokes_source_sessions_and_allows_password_totp_login`:
the spawned server was not yet listening after the test's 2-second wait. Both server-start waits in
`audit_install.rs` now allow 10 seconds and fail with a clear message instead of a connection error.

### Regression tests for an independent review

An independent review reported findings F01–F19; two verification reviews followed the fixes. Every finding has
reproducing tests that failed before its fix. Test names start with the finding id (and the related acceptance
check), e.g. `f04_t29_…`, `f07_…`, so results can be traced:

| Area | Test file | Findings |
|---|---|---|
| Access channels, party directory, redaction in decisions/history/audit/export, assignment paths, contact correction | `tests/audit_access.rs` | F01, F02, F03, F05, F06, F12, F13, F14 |
| Reviewed-version finalisation, stale notices after adjournment/cancellation, decision copies vs drafts, calendar across midnight | `tests/audit_judicial.rs` | F04, F07, F08, F13, F18 |
| Closure evidence and dates, safe retries of committing commands, next-step summary, intake numbering beyond 9999 | `tests/audit_workflow.rs` | F11, F13, F15, F17 |
| SMTP transport (fake TLS ESMTP server: failure, retry, stall, crash recovery), file checks and clamd hook (fake clamd: clean, infected, error, timeout), every file channel, maintenance commands on the real binary, restore, temporary-password gate | `tests/audit_install.rs` | F09, F10, F16, F19 |
| One new filing through intake → registration → adjournment → decision → copy → closure → reports | `tests/acceptance_t37.rs` | — |
| Re-audit of `6ace6b8`: retries after a revoked grant (decision create/finalise, dispatch) return no stored metadata and create nothing; a retry with unchanged access returns the same record; party-create retries show current contacts; `case.view_all` alone cannot change or create party records while `party.edit` can correct them; a hidden draft cannot be edited, withdrawn or finalised, a hidden finalised decision cannot be amended; the author with access still replaces the file | `tests/reaudit_regressions.rs` | R01, R02, R03 |
| Review of `136d796`: another importer with case access sees neither the title nor the filename of a judicial note in a document package (before or after commit, in the list, the batch or the commit response), cannot commit a package with restricted rows, still sees ordinary rows, and sees a restricted row only while a grant opens its document; ordinary packages stay shared | `tests/import_visibility_regression.rs` | R04 |

The first five tests in `tests/reaudit_regressions.rs` are the re-auditor's proposals, unchanged. On `6ace6b8` nine
of the eleven tests fail (all five proposals included); the two positive controls pass on both commits.
The first test in `tests/import_visibility_regression.rs` is the reviewer's R04 proposal, unchanged apart from a
lock that serialises the import tests (they share one process-wide import permit). On `136d796` all three tests
fail; the third (an ordinary package stays shared) fails there only on the new `can_commit` field.

Specification gaps closed in `dfd093d`, each with tests: cancelling an announced hearing records who must be told
and retires moot "new date" tasks (C08, `hearings_tasks.rs`); a hearing outcome links its minutes as an exact
document version, redacted for people without access (C10, `hearings_tasks.rs`); ending the responsible clerk's
assignment or deactivating the user clears the responsible officer, and the response states whether that person
can still open the case and why (C05, `audit_access.rs`); staff workload numbers open their cases or tasks with
CSV, and the undelivered-notices report ignores superseded invitations like the case screen does (§11,
`reports_search.rs`); a decision lists its issued copies (§5, `decisions_dispatch.rs`).

PDF checks are also exercised on committed realistic fixtures (`tests/fixtures/pdf/`, generated on macOS with an
embedded TrueType font, a Flate image and text output) which must stay `clean`.

## Clean installation, backup and restore (Docker)

`scripts/clean-install-check.sh` at `dfd093d`: **RESULT: PASS** (exit 0, 111 PASS lines, 0 FAIL lines). Steps:

1. Build the image from the repository `Dockerfile` (16 MB, linux/arm64).
2. Maintenance commands against an uninitialised volume (`create-user`, `grant`, `backup`, `verify-audit`) refuse
   and create nothing; production without `TCR_CLAMD` or explicit `TCR_AV=off` refuses to start.
3. Production container on a fresh volume (`TCR_AV=off`, stated in the log: no clamd in the isolated check).
4. Five fictional accounts created only with the documented `create-user`/`grant`/`revoke` commands, passwords
   on stdin; each command prints the data directory and installation id.
5. Over HTTP: password → TOTP enrolment → records API answers `403 password_change_required` → password change
   with a fresh TOTP code → records API works. Court unit, registry and room set up in Settings; filing
   registered as a case; a PDF with two versions; a restricted document with a grant; staff and judge
   assignments; a hearing; a draft decision.
6. Encrypted backup (`Backup created: 38 tables, 4 files, 44 audit events`), key moved off the volume.
7. Restore without `--yes` and with a wrong key refuse and leave the target empty.
8. Restore into a new empty volume; second container started on it. Compared through the API: cases, case
   details, assignments, documents and versions with downloaded SHA-256, restricted access, hearings, decision,
   users and permissions, installation id. `verify-audit` intact on both (44 events). Cookies from the source
   installation are rejected (`401`) on the restored one; users log in again with password + TOTP.
9. The documented Compose restore sequence also verified. Containers, volumes and the image removed.

Container memory after each phase (`docker stats`): about 1.2–1.3 MiB; cgroup peak about 22 MiB including the
maintenance commands run inside it.

## Browser runs (Chromium, fresh demo instance; rows not marked "Re-audit" or "Review" were run at `2ff9141`)

| Check | Result |
|---|---|
| Main scenario: filing with information request and supplement → registration → judge assignment → hearing 17 Nov, notice reviewed and sent to the local mailbox → adjournment to 19 Nov (unsent old notice shown as *Superseded*, new notice) → outcome → decision finalised with the bound file and version shown → decision copy → closure with evidence → reports drill-down | PASS |
| Party of a case without access: not visible in search, pickers or by id (404) | PASS |
| Restricted document title absent from decisions, history, search and the audit log for people without access | PASS |
| Finalising from an old tab after the draft file was replaced: conflict message, nothing finalised | PASS |
| Responsible officer: no control without permission; replacement with reason ends the previous officer's access | PASS |
| Superseded notice never reaches the mailbox; a draft cannot be chosen as a decision copy | PASS |
| Closing date before the latest status change or registration rejected | PASS |
| Shared contact: neutral explanation for a clerk, correction by the registry head | PASS |
| Case with only a future hearing not listed as "no next step" | PASS |
| Hearing 23:00–01:00 shown on both calendar days | PASS |
| Same times under browser time zones UTC, Pacific/Funafuti and America/Los_Angeles | PASS |
| Data and session after page reload and after server restart | PASS |
| 390×844: no horizontal page overflow on main screens; dialogs fit with the close button visible; compact header | PASS |
| Keyboard only: visible focus, sensible order, Enter/Escape, focus trapped in dialogs and restored on close, arrow keys on case tabs | PASS |
| Re-audit (`6148772`): clerk with generic decision permissions sees a judge's restricted draft as "Restricted document" with no edit/withdraw/finalise actions and an explanation; PATCH, withdraw and finalise return 403 and the draft is unchanged | PASS |
| Re-audit: registry head reduced to `case.view_all` sees participants and contacts without "Edit contact"; PATCH and POST `/parties` return 403, contact unchanged; with `party.edit` the button appears and a correction saves | PASS |
| Re-audit: new filing → registration (UI) → judge assignment → judge uploads a ruling, drafts and finalises it → clerk closes the case on that decision | PASS |
| Review (`cdb12c0`/`dfd093d`), R04: judge (given `import.run`) uploads a ZIP with an ordinary letter and a judicial note and sees both rows; the registry head opens the batch and sees the letter but "Restricted document" for the note, no commit button and an explanation; her commit via the API returns 403; after the judge commits, she sees the letter's document link and "restricted document" for the note, and the note itself returns 404 | PASS |
| Review, C08: cancelling a confirmed (previously adjourned) hearing names who will be told, then lists one open task per required participant on the hearing card | PASS |
| Review, C10: the judge records an outcome choosing the uploaded minutes in "Minutes / record"; the hearing card links that exact version | PASS |
| Review, C05: the registry head ends the responsible clerk's assignment; the summary says she no longer has access and the case has no responsible officer | PASS |
| Review, §11/§5: a workload count opens the officer's open tasks (including the new cancellation tasks) with a CSV link; the Decisions page has an "Issued copies" column | PASS |

Evidence: [`docs/demo/main-scenario.mp4`](docs/demo/main-scenario.mp4) (2:53, recorded at `2ff9141`) and the
screenshots in [`docs/demo/`](docs/demo).

## Not covered here

- Delivery through a real e-mail provider: verified against an in-process TLS ESMTP server only. The installation
  must test its provider (`TCR_SMTP_URL`).
- A real ClamAV `clamd`: verified against an in-process clamd protocol server only.
- Restore on a second physical machine: verified on a new Docker volume with a separate container.
- No OCR. Built-in PDF checks quarantine encrypted PDFs, attachments, active content and files beyond the
  inspection limits; a small share of ordinary PDFs can therefore be quarantined and must be re-saved.
