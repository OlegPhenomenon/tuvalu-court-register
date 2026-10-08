-- Tuvalu Court Register — full schema (see docs/ARCHITECTURE.md).
-- Instants: UTC text 'YYYY-MM-DDTHH:MM:SSZ'. Calendar dates: court-local 'YYYY-MM-DD'.

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- ---------------------------------------------------------------- reference data
CREATE TABLE court_units (
  id     INTEGER PRIMARY KEY,
  code   TEXT NOT NULL UNIQUE,
  name   TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE registries (
  id            INTEGER PRIMARY KEY,
  court_unit_id INTEGER NOT NULL REFERENCES court_units(id),
  series        TEXT NOT NULL UNIQUE,            -- e.g. DEMO-CIV
  name          TEXT NOT NULL,
  active        INTEGER NOT NULL DEFAULT 1
);

-- One row per (registry, year); incremented inside the registration transaction.
CREATE TABLE case_number_counters (
  registry_id INTEGER NOT NULL REFERENCES registries(id),
  year        INTEGER NOT NULL,
  last_seq    INTEGER NOT NULL,
  PRIMARY KEY (registry_id, year)
);

CREATE TABLE rooms (
  id            INTEGER PRIMARY KEY,
  court_unit_id INTEGER REFERENCES court_units(id),
  name          TEXT NOT NULL UNIQUE,
  location      TEXT,
  active        INTEGER NOT NULL DEFAULT 1
);

-- kind: case_category | intake_channel | origin_island | document_type | closure_basis
--       | hearing_type | participant_role | dispatch_method | relation_kind
CREATE TABLE ref_items (
  id     INTEGER PRIMARY KEY,
  kind   TEXT NOT NULL,
  code   TEXT NOT NULL,
  label  TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1,
  sort   INTEGER NOT NULL DEFAULT 0,
  UNIQUE (kind, code)
);

CREATE TABLE message_templates (
  id      INTEGER PRIMARY KEY,
  code    TEXT NOT NULL UNIQUE,
  name    TEXT NOT NULL,
  subject TEXT NOT NULL,          -- placeholders: {case_number} {case_title} {recipient} {hearing_local} {room} {court}
  body    TEXT NOT NULL,
  active  INTEGER NOT NULL DEFAULT 1
);

-- ---------------------------------------------------------------- users & access
CREATE TABLE users (
  id             INTEGER PRIMARY KEY,
  username       TEXT NOT NULL UNIQUE,
  display_name   TEXT NOT NULL,
  title          TEXT,
  email          TEXT,
  password_hash  TEXT NOT NULL,
  totp_secret    TEXT,                       -- base32; NULL = not enrolled
  totp_pending   TEXT,                       -- secret awaiting confirmation during enrolment
  totp_last_step INTEGER,                    -- last accepted TOTP step (replay protection)
  is_judge       INTEGER NOT NULL DEFAULT 0,
  persona        TEXT UNIQUE,                -- demo persona key (olga, elena, viktor, sergei, pavel)
  active         INTEGER NOT NULL DEFAULT 1,
  failed_logins  INTEGER NOT NULL DEFAULT 0,
  locked_until   TEXT,
  must_change_password INTEGER NOT NULL DEFAULT 0,
  created_at     TEXT NOT NULL,
  deactivated_at TEXT
);

CREATE TABLE user_permissions (
  user_id    INTEGER NOT NULL REFERENCES users(id),
  permission TEXT NOT NULL,
  granted_by INTEGER REFERENCES users(id),
  granted_at TEXT NOT NULL,
  PRIMARY KEY (user_id, permission)
);

CREATE TABLE sessions (
  token_hash   TEXT PRIMARY KEY,               -- sha256(token) hex; raw token only in the cookie
  user_id      INTEGER NOT NULL REFERENCES users(id),
  created_at   TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  mfa_ok       INTEGER NOT NULL DEFAULT 0,
  revoked_at   TEXT
);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE login_attempts (
  id       INTEGER PRIMARY KEY,
  username TEXT NOT NULL,
  ip       TEXT,
  at       TEXT NOT NULL,
  success  INTEGER NOT NULL
);
CREATE INDEX login_attempts_user_at ON login_attempts(username, at);

-- ---------------------------------------------------------------- parties
-- No uniqueness on name: equal names never merge people (C04).
CREATE TABLE parties (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL CHECK (kind IN ('person','organisation')),
  name          TEXT NOT NULL,
  contact_email TEXT,
  contact_phone TEXT,
  address       TEXT,
  island        TEXT,
  notes         TEXT,
  created_by    INTEGER REFERENCES users(id),
  created_at    TEXT NOT NULL,
  version       INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX parties_name ON parties(name);

-- ---------------------------------------------------------------- cases
CREATE TABLE cases (
  id                   INTEGER PRIMARY KEY,
  registry_id          INTEGER NOT NULL REFERENCES registries(id),
  year                 INTEGER NOT NULL,
  seq                  INTEGER,                        -- NULL for imported legacy numbers outside the series
  number               TEXT NOT NULL UNIQUE,
  legacy_number        TEXT UNIQUE,
  title                TEXT NOT NULL,
  category             TEXT NOT NULL,
  status               TEXT NOT NULL CHECK (status IN ('registered','active','on_hold','closed','reopened')),
  restricted           INTEGER NOT NULL DEFAULT 0,
  summary              TEXT,
  registered_date      TEXT NOT NULL,                  -- court-local date of registration (historical for imports)
  registered_at        TEXT NOT NULL,                  -- instant the record was created
  registered_by        INTEGER REFERENCES users(id),
  responsible_user_id  INTEGER REFERENCES users(id),
  closure_basis        TEXT,
  closure_note         TEXT,
  closed_date          TEXT,
  closed_at            TEXT,
  closed_by            INTEGER REFERENCES users(id),
  legal_hold           INTEGER NOT NULL DEFAULT 0,
  import_batch_id      INTEGER REFERENCES import_batches(id),
  historical_incomplete INTEGER NOT NULL DEFAULT 0,    -- imported with missing values (flagged, never invented)
  version              INTEGER NOT NULL DEFAULT 1,
  updated_at           TEXT NOT NULL,
  UNIQUE (registry_id, year, seq)
);
CREATE INDEX cases_status ON cases(status);

CREATE TRIGGER cases_no_delete BEFORE DELETE ON cases
BEGIN SELECT RAISE(ABORT, 'case_delete_forbidden'); END;

CREATE TABLE case_status_history (
  id          INTEGER PRIMARY KEY,
  case_id     INTEGER NOT NULL REFERENCES cases(id),
  from_status TEXT,
  to_status   TEXT NOT NULL,
  reason      TEXT,
  basis       TEXT,
  by_user     INTEGER REFERENCES users(id),
  at          TEXT NOT NULL,
  effective_date TEXT NOT NULL                  -- court-local date used by period reports
);
CREATE INDEX case_status_history_case ON case_status_history(case_id);

CREATE TABLE case_participations (
  id                      INTEGER PRIMARY KEY,
  case_id                 INTEGER NOT NULL REFERENCES cases(id),
  party_id                INTEGER NOT NULL REFERENCES parties(id),
  role                    TEXT NOT NULL,         -- ref_items participant_role
  representative_party_id INTEGER REFERENCES parties(id),
  representation_basis    TEXT,
  service_contact         TEXT,                  -- address/e-mail used for dispatch in THIS case
  active                  INTEGER NOT NULL DEFAULT 1,
  added_by                INTEGER REFERENCES users(id),
  added_at                TEXT NOT NULL,
  ended_at                TEXT,
  end_reason              TEXT
);
CREATE INDEX case_participations_case ON case_participations(case_id);

CREATE TABLE case_assignments (
  id          INTEGER PRIMARY KEY,
  case_id     INTEGER NOT NULL REFERENCES cases(id),
  user_id     INTEGER NOT NULL REFERENCES users(id),
  role        TEXT NOT NULL CHECK (role IN ('judge','clerk','service_officer','registry_head','other')),
  reason      TEXT NOT NULL,
  assigned_by INTEGER REFERENCES users(id),
  start_at    TEXT NOT NULL,
  end_at      TEXT,
  ended_by    INTEGER REFERENCES users(id),
  end_reason  TEXT
);
CREATE INDEX case_assignments_case ON case_assignments(case_id);
CREATE INDEX case_assignments_user_active ON case_assignments(user_id) WHERE end_at IS NULL;

CREATE TABLE case_relations (
  id           INTEGER PRIMARY KEY,
  from_case_id INTEGER NOT NULL REFERENCES cases(id),
  to_case_id   INTEGER NOT NULL REFERENCES cases(id),
  kind         TEXT NOT NULL,                    -- ref_items relation_kind: follow_up | related | ...
  note         TEXT,
  created_by   INTEGER REFERENCES users(id),
  created_at   TEXT NOT NULL,
  CHECK (from_case_id <> to_case_id),
  UNIQUE (from_case_id, to_case_id, kind)
);

-- ---------------------------------------------------------------- intake
CREATE TABLE intakes (
  id                     INTEGER PRIMARY KEY,
  reference              TEXT NOT NULL UNIQUE,   -- IN-YYYY-NNNN
  status                 TEXT NOT NULL CHECK (status IN ('received','needs_information','ready_for_registration','linked_to_case','returned_or_redirected','duplicate')),
  sender_party_id        INTEGER REFERENCES parties(id),
  sender_name            TEXT NOT NULL,
  channel                TEXT NOT NULL,          -- ref_items intake_channel
  origin_island          TEXT,                   -- ref_items origin_island (never implies jurisdiction)
  document_date          TEXT,
  received_date          TEXT NOT NULL,
  entered_at             TEXT NOT NULL,
  description            TEXT NOT NULL,
  is_paper_original      INTEGER NOT NULL DEFAULT 0,
  paper_location         TEXT,
  missing_items          TEXT,
  parent_intake_id       INTEGER REFERENCES intakes(id),   -- supplement to an earlier intake
  duplicate_of_intake_id INTEGER REFERENCES intakes(id),
  case_id                INTEGER REFERENCES cases(id),
  status_reason          TEXT,
  created_by             INTEGER REFERENCES users(id),
  version                INTEGER NOT NULL DEFAULT 1,
  updated_at             TEXT NOT NULL
);
CREATE INDEX intakes_status ON intakes(status);

CREATE TRIGGER intakes_no_delete BEFORE DELETE ON intakes
BEGIN SELECT RAISE(ABORT, 'intake_delete_forbidden'); END;

CREATE TABLE intake_messages (                  -- correspondence about missing items (versioned package history)
  id         INTEGER PRIMARY KEY,
  intake_id  INTEGER NOT NULL REFERENCES intakes(id),
  direction  TEXT NOT NULL CHECK (direction IN ('outgoing','incoming','note')),
  body       TEXT NOT NULL,
  dispatch_id INTEGER REFERENCES dispatches(id),
  created_by INTEGER REFERENCES users(id),
  created_at TEXT NOT NULL
);

-- ---------------------------------------------------------------- documents
CREATE TABLE documents (
  id                INTEGER PRIMARY KEY,
  case_id           INTEGER REFERENCES cases(id),
  intake_id         INTEGER REFERENCES intakes(id),
  title             TEXT NOT NULL,
  doc_type          TEXT NOT NULL,              -- ref_items document_type
  source            TEXT NOT NULL CHECK (source IN ('court','party','external')),
  source_party_id   INTEGER REFERENCES parties(id),
  document_date     TEXT,
  received_date     TEXT,
  visibility        TEXT NOT NULL CHECK (visibility IN ('administrative','party_material','restricted','judicial_note')),
  is_paper_original INTEGER NOT NULL DEFAULT 0,
  original_location TEXT,
  legal_hold        INTEGER NOT NULL DEFAULT 0,
  created_by        INTEGER NOT NULL REFERENCES users(id),
  created_at        TEXT NOT NULL,
  version           INTEGER NOT NULL DEFAULT 1,
  CHECK (case_id IS NOT NULL OR intake_id IS NOT NULL)
);
CREATE INDEX documents_case ON documents(case_id);
CREATE INDEX documents_intake ON documents(intake_id);

CREATE TRIGGER documents_no_delete BEFORE DELETE ON documents
BEGIN SELECT RAISE(ABORT, 'document_delete_forbidden'); END;

CREATE TABLE document_versions (
  id           INTEGER PRIMARY KEY,
  document_id  INTEGER NOT NULL REFERENCES documents(id),
  version_no   INTEGER NOT NULL,
  filename     TEXT NOT NULL,
  content_type TEXT NOT NULL,
  size_bytes   INTEGER NOT NULL,
  sha256       TEXT NOT NULL,
  storage_key  TEXT NOT NULL UNIQUE,
  scan_status  TEXT NOT NULL CHECK (scan_status IN ('clean','quarantined')),
  scan_note    TEXT,
  note         TEXT,
  uploaded_by  INTEGER NOT NULL REFERENCES users(id),
  uploaded_at  TEXT NOT NULL,
  UNIQUE (document_id, version_no)
);
CREATE INDEX document_versions_sha ON document_versions(sha256);

-- Stored files are immutable: a version row can never change its bytes or disappear.
CREATE TRIGGER document_versions_immutable BEFORE UPDATE OF document_id, version_no, sha256, storage_key, size_bytes ON document_versions
BEGIN SELECT RAISE(ABORT, 'document_version_immutable'); END;
CREATE TRIGGER document_versions_no_delete BEFORE DELETE ON document_versions
BEGIN SELECT RAISE(ABORT, 'document_version_delete_forbidden'); END;

-- Grants for restricted documents and shares of judicial notes.
CREATE TABLE document_grants (
  id          INTEGER PRIMARY KEY,
  document_id INTEGER NOT NULL REFERENCES documents(id),
  user_id     INTEGER NOT NULL REFERENCES users(id),
  reason      TEXT NOT NULL,
  granted_by  INTEGER NOT NULL REFERENCES users(id),
  granted_at  TEXT NOT NULL,
  revoked_at  TEXT,
  revoked_by  INTEGER REFERENCES users(id)
);
CREATE INDEX document_grants_doc_user ON document_grants(document_id, user_id);

-- ---------------------------------------------------------------- hearings
CREATE TABLE hearings (
  id                     INTEGER PRIMARY KEY,
  case_id                INTEGER NOT NULL REFERENCES cases(id),
  hearing_type           TEXT NOT NULL,          -- ref_items hearing_type
  status                 TEXT NOT NULL CHECK (status IN ('draft','scheduled','held','adjourned','cancelled')),
  starts_at              TEXT NOT NULL,          -- UTC, inclusive
  ends_at                TEXT NOT NULL,          -- UTC, exclusive
  room_id                INTEGER REFERENCES rooms(id),
  judge_user_id          INTEGER REFERENCES users(id),
  notes                  TEXT,
  previous_hearing_id    INTEGER REFERENCES hearings(id),  -- set on the new hearing created by adjournment
  adjourned_to_id        INTEGER REFERENCES hearings(id),  -- set on the old hearing
  status_reason          TEXT,                   -- adjournment / cancellation / not-held reason
  status_authorised_by   TEXT,                   -- who authorised the adjournment (free text, e.g. "Judge Viktor Lauti")
  conflict_override      INTEGER NOT NULL DEFAULT 0,
  override_reason        TEXT,
  override_by            INTEGER REFERENCES users(id),
  outcome_summary        TEXT,
  next_step              TEXT,
  outcome_recorded_by    INTEGER REFERENCES users(id),
  outcome_recorded_at    TEXT,
  created_by             INTEGER REFERENCES users(id),
  created_at             TEXT NOT NULL,
  version                INTEGER NOT NULL DEFAULT 1,
  CHECK (ends_at > starts_at)
);
CREATE INDEX hearings_time ON hearings(starts_at);
CREATE INDEX hearings_case ON hearings(case_id);

-- DB-level double-booking protection for judge and room ([start,end) intervals).
-- Only 'scheduled' hearings occupy a slot; an explicit override (permission + reason) is allowed past it.
CREATE TRIGGER hearings_no_overlap_ins BEFORE INSERT ON hearings
WHEN NEW.status = 'scheduled' AND NEW.conflict_override = 0 AND EXISTS (
  SELECT 1 FROM hearings h
  WHERE h.status = 'scheduled'
    AND h.starts_at < NEW.ends_at AND NEW.starts_at < h.ends_at
    AND ((NEW.room_id IS NOT NULL AND h.room_id = NEW.room_id)
      OR (NEW.judge_user_id IS NOT NULL AND h.judge_user_id = NEW.judge_user_id)))
BEGIN SELECT RAISE(ABORT, 'hearing_conflict'); END;

CREATE TRIGGER hearings_no_overlap_upd BEFORE UPDATE OF status, starts_at, ends_at, room_id, judge_user_id, conflict_override ON hearings
WHEN NEW.status = 'scheduled' AND NEW.conflict_override = 0 AND EXISTS (
  SELECT 1 FROM hearings h
  WHERE h.id <> NEW.id AND h.status = 'scheduled'
    AND h.starts_at < NEW.ends_at AND NEW.starts_at < h.ends_at
    AND ((NEW.room_id IS NOT NULL AND h.room_id = NEW.room_id)
      OR (NEW.judge_user_id IS NOT NULL AND h.judge_user_id = NEW.judge_user_id)))
BEGIN SELECT RAISE(ABORT, 'hearing_conflict'); END;

-- A scheduled/held hearing never moves: rescheduling is an adjournment that creates a new linked row.
CREATE TRIGGER hearings_time_frozen BEFORE UPDATE OF starts_at, ends_at, room_id, judge_user_id ON hearings
WHEN OLD.status <> 'draft'
  AND (NEW.starts_at IS NOT OLD.starts_at OR NEW.ends_at IS NOT OLD.ends_at
    OR NEW.room_id IS NOT OLD.room_id OR NEW.judge_user_id IS NOT OLD.judge_user_id)
BEGIN SELECT RAISE(ABORT, 'hearing_time_immutable'); END;

CREATE TRIGGER hearings_no_delete BEFORE DELETE ON hearings
BEGIN SELECT RAISE(ABORT, 'hearing_delete_forbidden'); END;

CREATE TABLE hearing_participants (
  id         INTEGER PRIMARY KEY,
  hearing_id INTEGER NOT NULL REFERENCES hearings(id),
  party_id   INTEGER REFERENCES parties(id),
  user_id    INTEGER REFERENCES users(id),
  role       TEXT NOT NULL,
  required   INTEGER NOT NULL DEFAULT 1,
  attended   INTEGER,                            -- NULL = not recorded
  CHECK (party_id IS NOT NULL OR user_id IS NOT NULL)
);
CREATE INDEX hearing_participants_hearing ON hearing_participants(hearing_id);

-- ---------------------------------------------------------------- tasks
CREATE TABLE tasks (
  id               INTEGER PRIMARY KEY,
  case_id          INTEGER REFERENCES cases(id),
  intake_id        INTEGER REFERENCES intakes(id),
  hearing_id       INTEGER REFERENCES hearings(id),
  kind             TEXT NOT NULL DEFAULT 'general',  -- general | renotify | follow_up | ...
  title            TEXT NOT NULL,
  description      TEXT,
  assignee_user_id INTEGER REFERENCES users(id),
  due_date         TEXT,                         -- entered by a human; never computed as a legal deadline
  status           TEXT NOT NULL CHECK (status IN ('open','done','cancelled','carried_forward')),
  result           TEXT,
  status_reason    TEXT,
  created_by       INTEGER REFERENCES users(id),
  created_at       TEXT NOT NULL,
  closed_by        INTEGER REFERENCES users(id),
  closed_at        TEXT,
  version          INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX tasks_case ON tasks(case_id);
CREATE INDEX tasks_assignee_open ON tasks(assignee_user_id) WHERE status = 'open';

-- ---------------------------------------------------------------- decisions
CREATE TABLE decisions (
  id                     INTEGER PRIMARY KEY,
  case_id                INTEGER NOT NULL REFERENCES cases(id),
  title                  TEXT NOT NULL,
  decision_date          TEXT,
  status                 TEXT NOT NULL CHECK (status IN ('draft','finalised','superseded','withdrawn')),
  status_reason          TEXT,                      -- withdrawal reason (drafts only)
  document_id            INTEGER NOT NULL REFERENCES documents(id),
  document_version_id    INTEGER NOT NULL REFERENCES document_versions(id),  -- exact revision the decision binds to
  hearing_id             INTEGER REFERENCES hearings(id),
  author_user_id         INTEGER NOT NULL REFERENCES users(id),
  finalised_by           INTEGER REFERENCES users(id),
  finalised_at           TEXT,
  amends_decision_id     INTEGER REFERENCES decisions(id),
  amendment_basis        TEXT,
  superseded_by_id       INTEGER REFERENCES decisions(id),
  signed_file_uploaded   INTEGER NOT NULL DEFAULT 0,  -- an uploaded signed scan; NOT a qualified e-signature
  created_at             TEXT NOT NULL,
  version                INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX decisions_case ON decisions(case_id);

-- A finalised decision's binding to its document revision is frozen.
CREATE TRIGGER decisions_finalised_frozen BEFORE UPDATE OF document_id, document_version_id, title, decision_date ON decisions
WHEN OLD.status <> 'draft'
BEGIN SELECT RAISE(ABORT, 'decision_finalised_immutable'); END;
CREATE TRIGGER decisions_no_delete BEFORE DELETE ON decisions
BEGIN SELECT RAISE(ABORT, 'decision_delete_forbidden'); END;

-- ---------------------------------------------------------------- dispatch (notices and copy packages)
CREATE TABLE dispatches (
  id                 INTEGER PRIMARY KEY,
  case_id            INTEGER REFERENCES cases(id),
  intake_id          INTEGER REFERENCES intakes(id),
  hearing_id         INTEGER REFERENCES hearings(id),
  kind               TEXT NOT NULL CHECK (kind IN ('notice','copies','information_request')),
  template_code      TEXT,
  recipient_party_id INTEGER REFERENCES parties(id),
  recipient_name     TEXT NOT NULL,
  method             TEXT NOT NULL,              -- ref_items dispatch_method: email | post | hand | collection
  address            TEXT,
  subject            TEXT NOT NULL,
  body               TEXT NOT NULL,
  purpose            TEXT,
  status             TEXT NOT NULL CHECK (status IN ('draft','queued','sent','failed','cancelled')),
  reviewed_by        INTEGER REFERENCES users(id),   -- recipient & composition preview confirmed
  reviewed_at        TEXT,
  prepared_by        INTEGER NOT NULL REFERENCES users(id),
  prepared_at        TEXT NOT NULL,
  queued_by          INTEGER REFERENCES users(id),
  queued_at          TEXT,
  sent_at            TEXT,
  failure_reason     TEXT,
  status_reason      TEXT,
  version            INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX dispatches_case ON dispatches(case_id);
CREATE INDEX dispatches_status ON dispatches(status);

CREATE TABLE dispatch_items (
  id                  INTEGER PRIMARY KEY,
  dispatch_id         INTEGER NOT NULL REFERENCES dispatches(id),
  document_version_id INTEGER NOT NULL REFERENCES document_versions(id),
  UNIQUE (dispatch_id, document_version_id)
);

-- Each send/retry is an attempt (technical result); never a new decision or case.
CREATE TABLE delivery_attempts (
  id                INTEGER PRIMARY KEY,
  dispatch_id       INTEGER NOT NULL REFERENCES dispatches(id),
  attempt_no        INTEGER NOT NULL,
  status            TEXT NOT NULL CHECK (status IN ('sent','failed')),
  technical_receipt TEXT,                        -- e.g. local mailbox message id
  detail            TEXT,
  at                TEXT NOT NULL,
  UNIQUE (dispatch_id, attempt_no)
);

-- Human confirmation of handover (separate from technical delivery).
CREATE TABLE delivery_confirmations (
  id          INTEGER PRIMARY KEY,
  dispatch_id INTEGER NOT NULL REFERENCES dispatches(id),
  kind        TEXT NOT NULL CHECK (kind IN ('technical_ack','human_handover')),
  note        TEXT NOT NULL,
  occurred_date TEXT,
  recorded_by INTEGER NOT NULL REFERENCES users(id),
  recorded_at TEXT NOT NULL
);

-- Separate legal assessment of service, made by an authorised human.
CREATE TABLE service_assessments (
  id          INTEGER PRIMARY KEY,
  dispatch_id INTEGER NOT NULL REFERENCES dispatches(id),
  assessment  TEXT NOT NULL CHECK (assessment IN ('served','not_served','undetermined')),
  basis       TEXT NOT NULL,
  assessed_by INTEGER NOT NULL REFERENCES users(id),
  assessed_at TEXT NOT NULL
);

-- Local mailbox: nothing leaves the server.
CREATE TABLE mailbox (
  id            INTEGER PRIMARY KEY,
  dispatch_id   INTEGER NOT NULL REFERENCES dispatches(id),
  attempt_no    INTEGER NOT NULL,
  to_address    TEXT NOT NULL,
  subject       TEXT NOT NULL,
  body          TEXT NOT NULL,
  attachments   TEXT NOT NULL,                   -- JSON [{filename, sha256, size_bytes}]
  delivered_at  TEXT NOT NULL
);

-- ---------------------------------------------------------------- idempotency
CREATE TABLE operation_keys (
  key         TEXT NOT NULL,
  user_id     INTEGER NOT NULL REFERENCES users(id),
  operation   TEXT NOT NULL,
  request_hash TEXT NOT NULL,                  -- sha256 of the request body; reuse with a different body → 409
  result_json TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  PRIMARY KEY (user_id, key)
);

-- ---------------------------------------------------------------- import / export
CREATE TABLE import_batches (
  id           INTEGER PRIMARY KEY,
  kind         TEXT NOT NULL CHECK (kind IN ('cases_csv','files_zip')),
  filename     TEXT NOT NULL,
  source_sha256 TEXT NOT NULL,
  storage_key  TEXT NOT NULL,                    -- original upload kept unchanged
  status       TEXT NOT NULL CHECK (status IN ('previewed','committed','rejected')),
  preview_json TEXT NOT NULL,
  result_json  TEXT,
  created_by   INTEGER NOT NULL REFERENCES users(id),
  created_at   TEXT NOT NULL,
  committed_by INTEGER REFERENCES users(id),
  committed_at TEXT
);

CREATE TABLE export_batches (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL CHECK (kind IN ('case_package','technical')),
  case_id       INTEGER REFERENCES cases(id),
  purpose       TEXT NOT NULL,
  manifest_json TEXT NOT NULL,
  sha256        TEXT NOT NULL,
  created_by    INTEGER REFERENCES users(id),
  created_at    TEXT NOT NULL
);

-- ---------------------------------------------------------------- audit (append-only, hash-chained)
CREATE TABLE audit_events (
  id          INTEGER PRIMARY KEY,
  at          TEXT NOT NULL,
  user_id     INTEGER REFERENCES users(id),
  action      TEXT NOT NULL,                     -- e.g. case.registered, hearing.adjourned, document.viewed_restricted
  entity_type TEXT NOT NULL,
  entity_id   INTEGER,
  case_id     INTEGER REFERENCES cases(id),
  summary     TEXT NOT NULL,
  details     TEXT NOT NULL DEFAULT '{}',        -- JSON
  ip          TEXT,
  prev_hash   TEXT NOT NULL,
  hash        TEXT NOT NULL
);
CREATE INDEX audit_case ON audit_events(case_id);
CREATE INDEX audit_user ON audit_events(user_id);

CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit_events
BEGIN SELECT RAISE(ABORT, 'audit_immutable'); END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit_events
BEGIN SELECT RAISE(ABORT, 'audit_immutable'); END;
