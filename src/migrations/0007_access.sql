-- Optimistic locking for case-specific role, representation and service contact edits.
ALTER TABLE case_participations ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
