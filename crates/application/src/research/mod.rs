//! research module use cases (spec §6 — owned by the module worker).
//! Lead proposals (§13.4, B.3.12) and field evidence (+reviews).

pub mod decisions;
pub mod evidence;
pub mod proposals;
pub mod update_proposals;

pub use decisions::decide;
pub use evidence::{
    EvidenceFilters, create_evidence, create_evidence_for_run_tx, create_evidence_tx, get_evidence,
    list_evidence, review_evidence,
};
pub use proposals::{create_proposals, create_proposals_tx, get_proposal, list_proposals};
pub use update_proposals::{create_update_proposal_tx, propose_update};
