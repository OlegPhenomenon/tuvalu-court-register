-- Dispatch slice: the court-local date a manual handover/posting actually happened,
-- recorded by record-sent (`at` stays the instant the row was written).
ALTER TABLE delivery_attempts ADD COLUMN occurred_date TEXT;
