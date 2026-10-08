-- Correlated directory-policy lookups must search party links rather than scan each table.
CREATE INDEX idx_participations_party ON case_participations(party_id);
CREATE INDEX idx_participations_representative ON case_participations(representative_party_id);
CREATE INDEX idx_intakes_sender_party ON intakes(sender_party_id);
CREATE INDEX idx_documents_source_party ON documents(source_party_id);
CREATE INDEX idx_dispatches_recipient_party ON dispatches(recipient_party_id);
