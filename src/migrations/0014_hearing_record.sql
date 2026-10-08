-- C10: a hearing outcome may reference the minutes / record as one exact document version of the
-- same case (validated by the hearings handler: visible, same case, clean, not a judicial note).
ALTER TABLE hearings ADD COLUMN record_version_id INTEGER REFERENCES document_versions(id);
