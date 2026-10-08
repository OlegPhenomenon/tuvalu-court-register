-- The closure is bound to immutable evidence; reopening retains this and the audit history.
ALTER TABLE cases ADD COLUMN basis_document_version_id INTEGER REFERENCES document_versions(id);
ALTER TABLE cases ADD COLUMN basis_decision_id INTEGER REFERENCES decisions(id);
ALTER TABLE cases ADD COLUMN basis_hearing_id INTEGER REFERENCES hearings(id);
