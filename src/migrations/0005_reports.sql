-- Preserve preview options separately from the original immutable source bytes.
ALTER TABLE import_batches ADD COLUMN source_options TEXT NOT NULL DEFAULT '{}';
CREATE INDEX status_history_effective ON case_status_history(case_id, effective_date, id);
CREATE INDEX documents_case_visibility ON documents(case_id, visibility);
