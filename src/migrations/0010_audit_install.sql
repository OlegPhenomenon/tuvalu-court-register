-- rebuild document_versions: preserve ids, immutable bytes, references and scan verdicts.
CREATE TABLE document_versions_install (
  id INTEGER PRIMARY KEY,
  document_id INTEGER NOT NULL REFERENCES documents(id),
  version_no INTEGER NOT NULL,
  filename TEXT NOT NULL,
  content_type TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  storage_key TEXT NOT NULL UNIQUE,
  scan_status TEXT NOT NULL CHECK (scan_status IN ('clean','pending_scan','quarantined')),
  scan_note TEXT,
  note TEXT,
  uploaded_by INTEGER NOT NULL REFERENCES users(id),
  uploaded_at TEXT NOT NULL,
  UNIQUE(document_id, version_no)
);
INSERT INTO document_versions_install SELECT * FROM document_versions;
UPDATE document_versions_install SET scan_note='Legacy format checks only, no antivirus verdict' WHERE scan_status='clean' AND scan_note IS NULL;
DROP TABLE document_versions;
ALTER TABLE document_versions_install RENAME TO document_versions;
CREATE INDEX document_versions_sha ON document_versions(sha256);
CREATE TRIGGER document_versions_immutable BEFORE UPDATE OF document_id, version_no, sha256, storage_key, size_bytes ON document_versions
BEGIN SELECT RAISE(ABORT, 'document_version_immutable'); END;
CREATE TRIGGER document_versions_no_delete BEFORE DELETE ON document_versions
BEGIN SELECT RAISE(ABORT, 'document_version_delete_forbidden'); END;
INSERT OR IGNORE INTO settings(key,value) VALUES('installation_id',lower(hex(randomblob(16))));
CREATE TABLE mail_retries (
  dispatch_id INTEGER PRIMARY KEY REFERENCES dispatches(id),
  retry_at TEXT NOT NULL
);
CREATE TABLE mail_delivery_log (
  mailbox_id INTEGER PRIMARY KEY REFERENCES mailbox(id),
  transport TEXT NOT NULL
);
