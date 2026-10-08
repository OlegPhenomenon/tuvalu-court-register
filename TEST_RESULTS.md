# Test results

Recorded 8 October 2026 on `main` at commit `2ff9141` (code identical to `5d4b6db`; `2ff9141` only re-records
`docs/demo/`). The requirement-by-requirement checklist is [`ACCEPTANCE.md`](ACCEPTANCE.md).

Tool versions: rustc/cargo 1.98.1, clippy 0.1.98, Node 23.10.0, npm 11.19.0, TypeScript 5.9.3, Vite 8.3.3,
Docker 29.4.3 (linux/arm64), Python 3.10.11. Host: macOS 26 (arm64).

## Build, lint, types

| Command | Result |
|---|---|
| `cargo build` | finished, 0 warnings |
| `cargo clippy --all-targets` | 0 warnings, 0 errors (`clippy.toml`: argument limit 8) |
| `cd web && npm run typecheck` | `tsc --noEmit`, no errors |
| `cd web && npm run build` | `dist/assets/index-*.css 18.8 kB`, `index-*.js 492 kB (gzip 141 kB)`, built |

## Automated tests (`cargo test`)

Per test binary, as printed by cargo:

| Test binary | Result |
|---|---|
| `src/lib.rs` (unit) | ok. 9 passed; 0 failed |
| `src/main.rs` | ok. 0 passed; 0 failed |
| `tests/acceptance_t37.rs` | ok. 1 passed; 0 failed |
| `tests/admin_seed.rs` | ok. 7 passed; 0 failed |
| `tests/audit_access.rs` | ok. 27 passed; 0 failed |
| `tests/audit_install.rs` | ok. 46 passed; 0 failed |
| `tests/audit_judicial.rs` | ok. 16 passed; 0 failed |
| `tests/audit_workflow.rs` | ok. 25 passed; 0 failed |
| `tests/backup.rs` | ok. 1 passed; 0 failed |
| `tests/decisions_dispatch.rs` | ok. 14 passed; 0 failed |
| `tests/documents.rs` | ok. 15 passed; 0 failed |
| `tests/e2e_backend.rs` | ok. 8 passed; 0 failed |
| `tests/hearings_tasks.rs` | ok. 22 passed; 0 failed |
| `tests/import_export.rs` | ok. 13 passed; 0 failed |
| `tests/intake_cases.rs` | ok. 11 passed; 0 failed |
| `tests/reports_search.rs` | ok. 4 passed; 0 failed |
| Doc-tests | ok. 0 passed; 0 failed |

219 tests passed, 0 failed. (One test in `audit_install.rs` re-runs the test binary as a child process to check
command-line behaviour; that child prints an extra `1 passed; 45 filtered out` line, which is not a separate test.)
The suite was run three times in a row without a failure.

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

PDF checks are also exercised on committed realistic fixtures (`tests/fixtures/pdf/`, generated on macOS with an
embedded TrueType font, a Flate image and text output) which must stay `clean`.

## Clean installation, backup and restore (Docker)

`scripts/clean-install-check.sh` at `2ff9141`: **RESULT: PASS** (exit 0, 0 FAIL lines). Steps:

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

## Browser runs (headless Chromium, fresh demo instance)

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

Evidence: [`docs/demo/main-scenario.mp4`](docs/demo/main-scenario.mp4) (2:53) and the screenshots in
[`docs/demo/`](docs/demo).

## Not covered here

- Delivery through a real e-mail provider: verified against an in-process TLS ESMTP server only. The installation
  must test its provider (`TCR_SMTP_URL`).
- A real ClamAV `clamd`: verified against an in-process clamd protocol server only.
- Restore on a second physical machine: verified on a new Docker volume with a separate container.
- No OCR. Built-in PDF checks quarantine encrypted PDFs, attachments, active content and files beyond the
  inspection limits; a small share of ordinary PDFs can therefore be quarantined and must be re-saved.
