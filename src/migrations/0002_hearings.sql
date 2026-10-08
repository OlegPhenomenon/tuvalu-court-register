-- Hearings slice: indexes for the calendar feed and the app-level conflict scan
-- (the schema and its triggers already live in 0001_init.sql).
CREATE INDEX hearings_room_time ON hearings(room_id, starts_at);
CREATE INDEX hearings_judge_time ON hearings(judge_user_id, starts_at);
CREATE INDEX tasks_hearing ON tasks(hearing_id);
