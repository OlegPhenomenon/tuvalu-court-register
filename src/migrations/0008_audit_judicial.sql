-- Rebuild the status constraint without changing ids or child references.
PRAGMA defer_foreign_keys = ON;
CREATE TEMP TABLE audit_dispatch_backup AS SELECT * FROM dispatches;
DROP TABLE dispatches;
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
  status             TEXT NOT NULL CHECK (status IN ('draft','queued','sent','failed','cancelled','superseded')),
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
INSERT INTO dispatches SELECT * FROM audit_dispatch_backup;
DROP TABLE audit_dispatch_backup;
CREATE INDEX dispatches_case ON dispatches(case_id);
CREATE INDEX dispatches_status ON dispatches(status);
ALTER TABLE dispatches ADD COLUMN hearing_version INTEGER;
ALTER TABLE dispatches ADD COLUMN hearing_starts_at TEXT;
ALTER TABLE dispatches ADD COLUMN notice_purpose TEXT NOT NULL DEFAULT 'invitation'
  CHECK (notice_purpose IN ('invitation','cancellation','other'));
ALTER TABLE dispatch_items ADD COLUMN material_kind TEXT NOT NULL DEFAULT 'working_document'
  CHECK (material_kind IN ('working_document','decision_copy'));
ALTER TABLE dispatch_items ADD COLUMN decision_id INTEGER REFERENCES decisions(id);
-- Existing pending items lack a provable review binding: the worker holds them.
UPDATE dispatch_items SET material_kind='decision_copy', decision_id=(
 SELECT d.id FROM decisions d WHERE d.document_version_id=dispatch_items.document_version_id
 AND d.status='finalised' ORDER BY d.id DESC LIMIT 1)
WHERE EXISTS (SELECT 1 FROM decisions d WHERE d.document_version_id=dispatch_items.document_version_id AND d.status='finalised');

-- Pending legacy working material must carry an unambiguous label after upgrade.
UPDATE dispatches SET subject='DRAFT / working material: ' || subject,
 body='DRAFT / working material' || char(10) || char(10) || body,
 reviewed_at=NULL, reviewed_by=NULL
WHERE status IN ('draft','queued','failed') AND EXISTS (
 SELECT 1 FROM dispatch_items di WHERE di.dispatch_id=dispatches.id AND di.material_kind='working_document');
INSERT OR IGNORE INTO message_templates(code,name,subject,body) VALUES (
 'hearing_cancellation','Hearing cancellation',
 '{court}: hearing cancelled in {case_number}',
 'Dear {recipient},

The hearing in case {case_number} listed for {hearing_local} has been cancelled. Do not attend that appointment. Please contact the registry for the next step.

{court}');
