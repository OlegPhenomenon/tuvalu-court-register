/**
 * Types mirroring GET /api/cases/:id exactly (src/api/cases.rs → case_json).
 * Every tab component takes `{ caseId, caseData, reload }`.
 */

export interface ClosureEvidence { label: string; link: string | null }

export interface CaseRecord {
  closure_evidence: ClosureEvidence | null;
  id: number;
  registry_id: number;
  year: number;
  seq: number | null;
  number: string;
  legacy_number: string | null;
  title: string;
  category: string;
  category_label: string;
  status: string;
  restricted: number;
  summary: string | null;
  registered_date: string;
  registered_at: string;
  registered_by: number | null;
  responsible_user_id: number | null;
  closure_basis: string | null;
  closure_note: string | null;
  closed_date: string | null;
  closed_at: string | null;
  closed_by: number | null;
  legal_hold: number;
  import_batch_id: number | null;
  historical_incomplete: number;
  version: number;
  updated_at: string;
  // joins added by case_json
  series: string;
  registry_name: string;
  registered_by_name: string | null;
  responsible_name: string | null;
  closed_by_name: string | null;
}

export interface Participant {
  version: number;
  id: number;
  party_id: number;
  kind: string;
  name: string;
  contact_email: string | null;
  contact_phone: string | null;
  address: string | null;
  role: string;
  active: number;
  representative_party_id: number | null;
  representative_name: string | null;
  representation_basis: string | null;
  service_contact: string | null;
  added_at: string;
  ended_at: string | null;
  end_reason: string | null;
}

export interface Assignment {
  id: number;
  user_id: number;
  display_name: string;
  title: string | null;
  role: string;
  reason: string;
  start_at: string;
  end_at: string | null;
  end_reason: string | null;
  assigned_by_name: string | null;
  ended_by_name: string | null;
}

export interface CaseRelation {
  id: number;
  kind: string;
  note: string | null;
  created_at: string;
  direction: 'outgoing' | 'incoming';
  other_case_id: number;
  other_number: string;
  other_title: string;
}

export interface StatusHistoryItem {
  closure_evidence: ClosureEvidence | null;
  from_status: string | null;
  to_status: string;
  reason: string | null;
  basis: string | null;
  at: string;
  effective_date: string | null;
  by_name: string | null;
}

export interface LinkedIntake {
  id: number;
  reference: string;
  received_date: string;
  sender_name: string;
  description: string;
  parent_intake_id: number | null;
}

export interface DecisionState {
  has_final_decision: number;
  draft_decisions: number;
}

export interface NextAction {
  code: string;
  message: string;
  link: string;
}

/** Server-computed permissions for this case (cases.rs `allowed`). */
export interface CaseAllowed {
  edit: boolean;
  assign_staff: boolean;
  assign_judge: boolean;
  close: boolean;
  reopen: boolean;
  set_status: boolean;
  schedule_hearing: boolean;
  record_outcome: boolean;
  manage_documents: boolean;
  draft_decision: boolean;
  finalise_decision: boolean;
  dispatch: boolean;
  manage_tasks: boolean;
  export: boolean;
  grant_restricted: boolean;
}

export interface CaseData {
  case: CaseRecord;
  participants: Participant[];
  assignments: Assignment[];
  relations: CaseRelation[];
  status_history: StatusHistoryItem[];
  intakes: LinkedIntake[];
  decision_state: DecisionState;
  next_actions: NextAction[];
  allowed: CaseAllowed;
}

export interface CaseTabProps {
  caseId: number;
  caseData: CaseData;
  /** Re-fetch GET /api/cases/:id after a mutation. */
  reload: () => void;
}

/** One blocking item returned in 409 `open_items` when closing a case. */
export interface OpenItem {
  kind: 'task' | 'hearing' | 'dispatch' | 'decision' | string;
  id: number;
  label: string;
  status: string;
}
