# Test results

Recorded 8 October 2026 on `main`. The checks are mapped to requirements in [`ACCEPTANCE.md`](ACCEPTANCE.md).

## Automated tests

Commands:

```sh
cargo build            # 0 warnings
cargo test             # all suites below
cd web && npm run typecheck && npm run build
```

| Suite | Passed | Failed | Covers |
|---|---:|---:|---|
| `src/` unit tests | 9 | 0 | court time (UTC+12), TOTP (RFC 6238 vector), password hashing, file-type detection/quarantine, filename sanitising |
| `tests/intake_cases.rs` | 11 | 0 | intake states, supplements, registration, parallel numbering, idempotent replay, visibility by assignment, restricted cases, version conflicts, closing/reopening, party non-merging, audit immutability, sandbox isolation, CSRF |
| `tests/hearings_tasks.rs` | 21 | 0 | scheduling, `[start,end)` conflicts, override permission, concurrent confirmation, adjournment chain, outcomes, corrections, tasks, close blocking |
| `tests/documents.rs` | 15 | 0 | uploads, refused types, quarantine, versions, restricted grants, judicial notes, revoked access to old URLs, download headers |
| `tests/decisions_dispatch.rs` | 14 | 0 | decision draft/finalise/withdraw/amend, notices and copy packages, mandatory review, outbox re-checks, failures and retries, handover vs service assessment, mailbox visibility |
| `tests/reports_search.rs` | 4 | 0 | policy-filtered counts, event-dated periods, drill-down, CSV formula escaping, search |
| `tests/import_export.rs` | 13 | 0 | legacy CSV preview/commit, historical dates, number high-water mark, ZIP safety, case export contents |
| `tests/backup.rs` | 1 | 0 | encrypted backup → verified restore, wrong key, tampering, truncation, non-empty target |
| `tests/admin_seed.rs` | 7 | 0 | admin escalation limits, deactivation, credential resets, settings validation, DEMO dataset invariants |
| `tests/e2e_backend.rs` | 8 | 0 | regressions found by the UI walkthrough (closing with unconfirmed notices, information requests, assignability, audit of restricted views, mailbox redaction, adjournment rules) |
| **Total** | **103** | **0** | |

No automated check failed.

## End-to-end walkthrough in a browser

The main scenario from spec §3 was run through the real UI in headless Chromium (1440×900) against a fresh demo instance. The first run found 12 defects, which were fixed. The second run, re-testing all 12 and the full scenario three times, found no defects.

| Step | Result |
|---|---|
| Olga receives a filing, requests information, adds a supplement, registers DEMO-CIV-2026-0004 | PASS |
| Elena assigns the judge (Viktor) and a service officer (Sergei) | PASS |
| Hearing on 17 Nov 2026 09:00, notices reviewed and sent to the local mailbox | PASS |
| Sergei: mailbox and handover confirmation; restricted document invisible to him (404 on download) | PASS |
| Party document with two versions and checksums | PASS |
| Adjournment to 19 Nov 2026 with reason and authoriser; linked records; re-notification tasks | PASS |
| Viktor records the outcome, then drafts and finalises the decision | PASS |
| Separate copy packages to each party; closing blocked until open items are resolved or explained; case closed | PASS |
| Reopening, reports drill-down, calendar, history, Pavel without case access, global search, demo reset | PASS |

Evidence: [`docs/demo/main-scenario.mp4`](docs/demo/main-scenario.mp4) and the screenshots in [`docs/demo/`](docs/demo).

## Deployment checks

- The Docker image (`Dockerfile`, Alpine, static musl binary) builds in about 2.5 minutes. The image is 14.8 MB.
- The container answered `/api/health` and served the SPA. Five demo sandboxes started and signed in, using 2.6 MB of disk in total.

## Not covered

- No OCR and no antivirus engine. Files are checked by content type, and active PDF/DOCX content is quarantined.
- Real e-mail delivery is disabled by design. Messages go to the local mailbox.
- The backup and restore round trip is tested automatically in production mode. A restore onto a separate clean server has not been run.
