-- Keep each closure's evidence even after reopening and closing again.
ALTER TABLE case_status_history ADD COLUMN basis_document_version_id INTEGER REFERENCES document_versions(id);
ALTER TABLE case_status_history ADD COLUMN basis_decision_id INTEGER REFERENCES decisions(id);
ALTER TABLE case_status_history ADD COLUMN basis_hearing_id INTEGER REFERENCES hearings(id);
UPDATE case_status_history AS h SET
  basis_document_version_id = json_extract(a.details, '$.basis_document_version_id'),
  basis_decision_id = json_extract(a.details, '$.basis_decision_id'),
  basis_hearing_id = json_extract(a.details, '$.basis_hearing_id')
FROM audit_events a WHERE h.to_status='closed' AND a.action='case.closed' AND a.case_id=h.case_id
AND h.effective_date=json_extract(a.details,'$.closed_date')
AND (SELECT COUNT(*) FROM case_status_history h2 WHERE h2.case_id=h.case_id AND h2.to_status='closed' AND h2.id>=h.id)
  = (SELECT COUNT(*) FROM audit_events a2 WHERE a2.case_id=a.case_id AND a2.action='case.closed' AND a2.id>=a.id);
-- Older demo/import closures may have no evidence references in their audit event.
UPDATE case_status_history AS h SET
  basis_document_version_id=c.basis_document_version_id,
  basis_decision_id=c.basis_decision_id,
  basis_hearing_id=c.basis_hearing_id
FROM cases c WHERE h.case_id=c.id AND h.to_status='closed'
AND h.id=(SELECT MAX(id) FROM case_status_history WHERE case_id=c.id AND to_status='closed')
AND h.basis_document_version_id IS NULL AND h.basis_decision_id IS NULL AND h.basis_hearing_id IS NULL;
-- Auto-created re-notification tasks are bound to a party, never matched by a mutable title.
ALTER TABLE tasks ADD COLUMN renotify_party_id INTEGER REFERENCES parties(id);
-- Recover bindings for earlier automatically generated tasks where the party is unambiguous.
UPDATE tasks AS t SET renotify_party_id=(
  SELECT hp.party_id FROM hearing_participants hp JOIN parties p ON p.id=hp.party_id
  WHERE hp.hearing_id=t.hearing_id AND hp.required=1
    AND substr(t.title,1,length('Notify ' || p.name || ' of the new hearing date ('))='Notify ' || p.name || ' of the new hearing date ('
) WHERE t.kind='renotify'
AND EXISTS(SELECT 1 FROM audit_events a, json_each(a.details,'$.tasks') task
  WHERE a.action='hearing.adjourned' AND task.value=t.id)
AND 1=(SELECT COUNT(*) FROM hearing_participants hp JOIN parties p ON p.id=hp.party_id
  WHERE hp.hearing_id=t.hearing_id AND hp.required=1
    AND substr(t.title,1,length('Notify ' || p.name || ' of the new hearing date ('))='Notify ' || p.name || ' of the new hearing date (');
