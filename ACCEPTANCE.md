# Acceptance checklist — Tuvalu Court Register

Derived from `docs/spec/SPEC_RU.txt` §3–§4, §7–§12, §14 and features C01–C18.
Every check names the automated test(s) that cover it, or "manual" where only
the UI walkthrough demonstrates it (`docs/demo/`, `docs/demo/main-scenario.mp4`).

Run all automated checks with `cargo test` — every test drives the real HTTP
router against a temporary data directory; no external services needed.

## A. Case lifecycle (spec §3)

| # | Check | Covered by |
|---|---|---|
| 1 | An intake is recorded with sender, channel, dates, description, attachments and paper location, and is **not** a case until registered | `tests/intake_cases.rs::intake_is_not_a_case_until_registered` |
| 2 | Request-information sets `needs_information` and prepares a **draft** message; a supplement links to the original intake and returns it to `received`; a supplement cannot be registered alone | `tests/intake_cases.rs::intake_is_not_a_case_until_registered` |
| 3 | Registration requires the explicit ready step and issues a unique `<series>-<year>-<seq>` number; replaying the request never creates a second case | `tests/intake_cases.rs::registration_numbers_are_unique_and_idempotent` |
| 4 | Two clerks registering simultaneously cannot obtain the same number | `tests/intake_cases.rs::parallel_registration_on_multi_thread_runtime` |
| 5 | Same-name parties are never merged automatically | `tests/intake_cases.rs::same_name_parties_are_never_merged` |
| 6 | Judge assignment is made by an authorised person and re-checked when a hearing is confirmed | `tests/hearings_tasks.rs::confirm_rechecks_judge_assignment_and_override_is_audited` |
| 7 | Adjournment creates a linked new hearing, frees the old slot, keeps reason + authoriser, and creates re-notification tasks | `tests/hearings_tasks.rs::adjourn_links_new_hearing_and_frees_the_slot`, `adjourn_conflict_rolls_back_and_renotifies_only_required_participants` |
| 8 | A held hearing records attendance, summary and next step; it does not close the case; a not-held outcome keeps the record with a reason | `tests/hearings_tasks.rs::outcome_held_with_attendance_task_and_next_hearing`, `outcome_not_held_needs_reason_and_keeps_the_record` |
| 9 | Decisions go draft → finalised → superseded via a linked amendment; finalisation is atomic and idempotent | `tests/decisions_dispatch.rs::decision_finalisation_amendment_and_replay_are_atomic` |
| 10 | Copy packages are per-recipient, exact versions only, with mandatory preview before queuing | `tests/decisions_dispatch.rs::package_validation_and_review_invalidation`, `copies_review_queue_mailbox_and_delivery_records` |
| 11 | Closing requires a basis and fails with a list of open items until each is resolved; reopen requires a reason | `tests/intake_cases.rs::closing_requires_basis_and_no_open_items_then_reopen`, `tests/hearings_tasks.rs::open_items_block_closing_until_resolved` |
| 12 | Intake can be marked duplicate / returned / linked to an existing case with a recorded reason; intakes are never deleted | manual — see docs/demo |

## B. Situations outside the main flow (spec §4)

| # | Check | Covered by |
|---|---|---|
| 13 | A case can be closed without a hearing using an authorised closure basis | `tests/intake_cases.rs::closing_requires_basis_and_no_open_items_then_reopen` |
| 14 | Document date, received date and registration date are stored separately; origin island is a reference value, not a jurisdiction decision | `tests/import_export.rs::legacy_numbers_dates_and_validation`; manual — intake form |
| 15 | A replaced/departed staff member loses all access: sessions ended, assignments closed, old file URLs dead | `tests/admin_seed.rs::deactivate_ends_sessions_and_assignments`, `tests/documents.rs::ended_assignment_breaks_old_download_urls` |
| 16 | A restricted case is invisible in lists, search, counts and history for unauthorised staff | `tests/intake_cases.rs::restricted_case_hidden_from_view_all`, `tests/reports_search.rs::hidden_counterpart_case_is_redacted_in_history_and_audit` |
| 17 | A finalised decision version cannot be replaced; corrections are linked amendments | `tests/documents.rs::finalised_decision_freezes_the_document`, `tests/decisions_dispatch.rs::decision_finalisation_amendment_and_replay_are_atomic` |
| 18 | Failed submission keeps entered text with a "Not saved — retry" state; no persistent offline copy of case data; retry does not duplicate records | `tests/documents.rs::idempotent_upload_replay`, `tests/intake_cases.rs::registration_numbers_are_unique_and_idempotent`; UI behaviour: manual — see docs/demo |

## C. Statuses, calendar, numbering (spec §7–§8)

| # | Check | Covered by |
|---|---|---|
| 19 | Intake/case/hearing/decision/dispatch/task state machines reject invalid transitions with `invalid_transition` | exercised throughout; e.g. `tests/intake_cases.rs::intake_is_not_a_case_until_registered`, `tests/decisions_dispatch.rs::decision_edits_withdrawal_and_close_blockers` |
| 20 | Hearing interval is `[start, end)`; boundary times do not conflict | `tests/hearings_tasks.rs::draft_patch_confirm_and_boundary` |
| 21 | Double-booking a judge or room is blocked in the database; parallel confirms let exactly one win | `tests/hearings_tasks.rs::concurrent_confirmation_lets_one_win`, `room_only_conflicts_and_buffer_on_both_sides` |
| 22 | A conflict override needs `hearing.override_conflict` and a reason, and is audited | `tests/hearings_tasks.rs::conflict_override_requires_permission` |
| 23 | A held hearing cannot be adjourned; erroneous outcomes are corrected via `hearing.admin_correct` with a reason | `tests/hearings_tasks.rs::outcome_followups_are_atomic_and_correction_checks_conflicts` |
| 24 | Production refuses an outcome before the hearing start time (demo allows it for the walkthrough) | `tests/hearings_tasks.rs::production_rejects_future_outcomes_but_accepts_past_hearings` |
| 25 | Calendar works on court-local dates, independent of the browser timezone, with day/judge/room filters | `tests/hearings_tasks.rs::calendar_uses_court_dates_and_all_filters` |
| 26 | Concurrent edits produce `version_conflict` with the current record instead of silently overwriting | `tests/intake_cases.rs::concurrent_edit_reports_version_conflict` |

## D. Access control and protection (spec §9–§10, C01/C05/C15)

| # | Check | Covered by |
|---|---|---|
| 27 | Production login = password (Argon2id, ≥12 chars) + TOTP second factor, enrolled at first login, codes non-replayable | `tests/admin_seed.rs::production_admin_account_lifecycle`, unit `src/auth.rs::tests::totp_rfc6238_vector` |
| 28 | Lockout after repeated failures; sessions revocable; rotation after MFA | `tests/admin_seed.rs::production_admin_account_lifecycle`, `admin_user_management_in_demo` |
| 29 | The technical administrator has no access to cases, files or judicial notes and cannot grant judicial/case-access permissions from the UI | `tests/documents.rs::tech_admin_sees_nothing`, `tests/admin_seed.rs::admin_denied_without_permission` |
| 30 | Case visibility = assignment / `view_all` on non-restricted / `view_restricted`, applied to lists, search, counts, downloads and the worker | `tests/intake_cases.rs::case_visibility_follows_assignments`, `tests/hearings_tasks.rs::hearings_follow_case_visibility`, `tests/decisions_dispatch.rs::outbox_rechecks_assignment_and_restricted_grants` |
| 31 | Hidden objects return 404, never 403 | asserted by access tests, e.g. `tests/documents.rs::ended_assignment_breaks_old_download_urls`, `tests/intake_cases.rs::restricted_case_hidden_from_view_all` |
| 32 | Restricted documents need an explicit grant; every view is audited | `tests/documents.rs::restricted_document_grant_revoke_and_view_audit` |
| 33 | Judicial notes are private to the author until explicitly shared; the admin role cannot read them | `tests/documents.rs::judicial_note_is_private_to_the_author_until_shared` |
| 34 | Every non-GET request requires `X-TCR`; a mismatched Origin is rejected; cookies are `SameSite=Strict; HttpOnly` | `tests/intake_cases.rs::csrf_header_required_for_mutations` |
| 35 | Uploads are type-checked by magic bytes; HTML/SVG/executables refused; mislabelled extensions rejected | `tests/documents.rs::upload_rejects_dangerous_and_mislabelled_files`, unit `src/storage.rs::tests::rejects_html_and_svg` |
| 36 | PDFs with active content and macro/oversized DOCX are quarantined, never served | `tests/documents.rs::pdf_with_active_content_is_quarantined` |
| 37 | Oversized uploads rejected; failed writes leave no orphan blobs | `tests/documents.rs::oversized_and_closed_case_uploads_are_rejected`, `uploads_validate_before_storage_and_discard_after_failed_writes` |
| 38 | Audit log is append-only (UPDATE/DELETE blocked by triggers) and hash-chained | `tests/intake_cases.rs::audit_chain_and_immutability` |

## E. Reports, search, import, export, backup (spec §11, §14; C14/C16/C17)

| # | Check | Covered by |
|---|---|---|
| 39 | Reports use real event dates, count only visible cases, support drill-down and CSV | `tests/reports_search.rs::reports_dates_visibility_drilldown_and_csv`, `hearings_dispatch_and_workload_are_case_filtered` |
| 40 | Search and history never expose sensitive materials to unauthorised users | `tests/reports_search.rs::search_and_history_redact_sensitive_materials` |
| 41 | CSV import is preview → conflict report → idempotent commit; original registration dates and legacy numbers preserved; incomplete rows flagged, not invented | `tests/import_export.rs::csv_preview_commit_history_idempotency_and_numbering`, `legacy_numbers_dates_and_validation` |
| 42 | File-package import validates the manifest and rejects traversal paths, symlinks, duplicate entries, over-expansion and corrupt data | `tests/import_export.rs::zip_import_safety_policy_and_commit`, `zip_symlinks_duplicates_and_declared_expansion_limits_are_rejected`, `corrupt_zip_data_and_reduced_limits_are_rejected` |
| 43 | Re-importing the same content never creates duplicate cases or files | `tests/import_export.rs::repeated_zip_content_is_skipped_in_new_and_previously_previewed_batches` |
| 44 | Imported judicial notes require a judge and matching visibility; closed rows require closure data | `tests/import_export.rs::import_notes_require_judge_and_matching_visibility_and_closed_rows_require_dates` |
| 45 | Case export contains only versions the requester may see, with checksums, chronology and relations — and is not a backup | `tests/import_export.rs::export_only_permitted_versions_and_checksums`, `export_manifest_preserves_versions_times_relations_and_redacted_history`, `chronology_excludes_notes_and_restricted_events_outside_package_even_for_author` |
| 46 | Encrypted backup restores only into an empty directory after verifying authentication, schema, row counts, file checksums and the audit head | `tests/backup.rs::encrypted_backup_restore_checks_all_data_and_failure_paths` |

## F. Dispatch and mailbox (C09, C12)

| # | Check | Covered by |
|---|---|---|
| 47 | Notices are built from templates over case participants; queued → sent/failed; a retry is a new attempt row | `tests/decisions_dispatch.rs::notices_use_case_participants_and_hearing_templates`, `address_failure_retry_and_cancellation` |
| 48 | Technical delivery, human confirmation and the legal service assessment are separate records; manual methods require review and record the actual date | `tests/decisions_dispatch.rs::copies_review_queue_mailbox_and_delivery_records`, `manual_methods_need_review_and_record_actual_date` |
| 49 | The outbox worker re-checks current permissions and grants before delivery | `tests/decisions_dispatch.rs::outbox_rechecks_assignment_and_restricted_grants`, `outbox_rechecks_active_user_permission_and_document_state` |
| 50 | Nothing leaves the server — deliveries land in the local mailbox; restricted materials stay redacted there | `tests/decisions_dispatch.rs::copies_review_queue_mailbox_and_delivery_records`, `restricted_dispatch_and_mailbox_inventory_is_redacted_and_patch_keeps_cover_letter` |
| 51 | Concurrent finalisation + queuing + workers never duplicate history or attempts | `tests/decisions_dispatch.rs::concurrent_finalisation_queue_and_workers_do_not_duplicate_history` |
| 52 | Intake-level dispatches follow intake access even after the intake is linked to a case | `tests/decisions_dispatch.rs::intake_dispatches_follow_intake_access_even_after_linking` |

## G. Demo installation (spec §12, C18)

| # | Check | Covered by |
|---|---|---|
| 53 | The seeded dataset contains the required fictional set: incomplete intake, active case, adjourned hearing, closed case, reopened case, restricted-files case — all `DEMO`-marked | `tests/admin_seed.rs::demo_seed_dataset_is_complete_and_consistent` |
| 54 | Sandboxes are isolated: a visitor cannot see, reset or evict another visitor's data | `tests/intake_cases.rs::sandboxes_are_isolated` |
| 55 | Sandbox TTL/cap/quota enforced; heavy operations serialise across sandboxes | `tests/import_export.rs::heavy_operations_are_busy_across_sandboxes_and_release_after_upload`, `import_quota_counts_sources_and_failed_transactions_discard_all_blobs` |
| 56 | A visitor can complete the full scenario themselves in the UI (persona switch, intake → closure) without a pre-made card | manual — see `docs/demo/main-scenario.mp4` |
| 57 | Users, rooms, registries, reference lists, templates and settings are editable without the developer | `tests/admin_seed.rs::admin_reference_data`, `admin_settings_validation`, `admin_user_management_in_demo` |
| 58 | Install/upgrade/backup/restore verified on a clean environment | `tests/backup.rs::encrypted_backup_restore_checks_all_data_and_failure_paths` + manual — follow `docs/OPERATIONS.md` on a fresh server |

## Notes

- Items marked "manual" are UI-layer behaviours verified in the recorded
  walkthrough rather than in code-level tests.
- `verify-audit` output and the backup/restore round trip should be re-run on
  the real production database after installation (see `docs/OPERATIONS.md`).
