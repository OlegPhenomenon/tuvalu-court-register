-- Durable delivery attempts: a claim commits before attachment reads or SMTP.
-- No tables reference delivery_attempts; preserve all existing attempt ids/history.
CREATE TABLE delivery_attempts_claim (
  id INTEGER PRIMARY KEY,
  dispatch_id INTEGER NOT NULL REFERENCES dispatches(id),
  attempt_no INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('in_flight','sent','failed')),
  technical_receipt TEXT,
  detail TEXT,
  at TEXT NOT NULL,
  occurred_date TEXT,
  claim TEXT UNIQUE,
  message_id TEXT,
  attachments_json TEXT,
  dispatch_version INTEGER,
  CHECK (status != 'in_flight' OR
    (claim IS NOT NULL AND message_id IS NOT NULL AND attachments_json IS NOT NULL AND dispatch_version IS NOT NULL)),
  UNIQUE(dispatch_id, attempt_no)
);
INSERT INTO delivery_attempts_claim(id,dispatch_id,attempt_no,status,technical_receipt,detail,at,occurred_date)
  SELECT id,dispatch_id,attempt_no,status,technical_receipt,detail,at,occurred_date FROM delivery_attempts;
DROP TABLE delivery_attempts;
ALTER TABLE delivery_attempts_claim RENAME TO delivery_attempts;
CREATE UNIQUE INDEX delivery_one_in_flight ON delivery_attempts(dispatch_id) WHERE status='in_flight';
