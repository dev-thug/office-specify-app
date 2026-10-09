use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

/// Request context (spec §4.1). Workspace/principal come only from the
/// verified session or service credential — never from client input.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub request_id: String,
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub principal_kind: PrincipalKind,
    pub role: Role,
    pub scopes: ScopeSet,
    pub permission_version: i64,
    pub auth: AuthMethod,
    pub agent_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Human,
    Agent,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Manager,
    Member,
    Agent,
    System,
}

impl Role {
    /// Role-implied scopes for human principals (spec §4.6). Agent/system
    /// principals carry explicit scopes from their service credential.
    pub fn implied_scopes(self) -> ScopeSet {
        use Scope::*;
        let all = [
            CrmRead,
            CrmWrite,
            CrmPropose,
            CrmImport,
            CrmExport,
            ResearchPropose,
            ResearchWrite,
            TemplatesRead,
            TemplatesWrite,
            MailRead,
            MailCompose,
            MailSend,
            MailApprove,
            MailAssign,
            CampaignCompose,
            CampaignPause,
            BookingRead,
            BookingPropose,
            ApprovalsRead,
            OperationsRead,
            AgentsRun,
        ];
        let set: &[Scope] = match self {
            Role::Admin => &all,
            Role::Manager => &[
                CrmRead,
                CrmWrite,
                CrmPropose,
                CrmImport,
                CrmExport,
                ResearchPropose,
                ResearchWrite,
                TemplatesRead,
                TemplatesWrite,
                MailRead,
                MailCompose,
                MailSend,
                MailApprove,
                MailAssign,
                CampaignCompose,
                CampaignPause,
                BookingRead,
                BookingPropose,
                ApprovalsRead,
                OperationsRead,
                AgentsRun,
            ],
            // §5.3: members edit own/unassigned CRM records (crm_can_edit
            // still enforces ownership), reply under a send grant, compose
            // campaign drafts under a compose grant, pause campaigns in
            // their own scope, and view their own activity. They never get
            // approve/grant/admin scopes.
            Role::Member => &[
                CrmRead,
                CrmWrite,
                CrmPropose,
                ResearchPropose,
                TemplatesRead,
                MailRead,
                MailCompose,
                MailSend,
                CampaignCompose,
                CampaignPause,
                BookingRead,
                BookingPropose,
                ApprovalsRead,
                OperationsRead,
            ],
            Role::Agent | Role::System => &[],
        };
        ScopeSet::of(set.iter().copied())
    }
}

/// Bearer-credential scopes (spec §4.6). Agents never receive approve/grant/
/// send scopes; service credentials carry explicit subsets.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
pub enum Scope {
    #[serde(rename = "crm.read")]
    CrmRead,
    #[serde(rename = "crm.write")]
    CrmWrite,
    #[serde(rename = "crm.propose")]
    CrmPropose,
    #[serde(rename = "crm.import")]
    CrmImport,
    #[serde(rename = "crm.export")]
    CrmExport,
    #[serde(rename = "research.propose")]
    ResearchPropose,
    #[serde(rename = "research.write")]
    ResearchWrite,
    #[serde(rename = "templates.read")]
    TemplatesRead,
    #[serde(rename = "templates.write")]
    TemplatesWrite,
    #[serde(rename = "mail.read")]
    MailRead,
    #[serde(rename = "mail.compose")]
    MailCompose,
    #[serde(rename = "mail.send")]
    MailSend,
    #[serde(rename = "mail.approve")]
    MailApprove,
    #[serde(rename = "mail.assign")]
    MailAssign,
    #[serde(rename = "campaign.compose")]
    CampaignCompose,
    #[serde(rename = "campaign.pause")]
    CampaignPause,
    #[serde(rename = "booking.read")]
    BookingRead,
    #[serde(rename = "booking.propose")]
    BookingPropose,
    #[serde(rename = "approvals.read")]
    ApprovalsRead,
    #[serde(rename = "operations.read")]
    OperationsRead,
    #[serde(rename = "agents.run")]
    AgentsRun,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        use Scope::*;
        match self {
            CrmRead => "crm.read",
            CrmWrite => "crm.write",
            CrmPropose => "crm.propose",
            CrmImport => "crm.import",
            CrmExport => "crm.export",
            ResearchPropose => "research.propose",
            ResearchWrite => "research.write",
            TemplatesRead => "templates.read",
            TemplatesWrite => "templates.write",
            MailRead => "mail.read",
            MailCompose => "mail.compose",
            MailSend => "mail.send",
            MailApprove => "mail.approve",
            MailAssign => "mail.assign",
            CampaignCompose => "campaign.compose",
            CampaignPause => "campaign.pause",
            BookingRead => "booking.read",
            BookingPropose => "booking.propose",
            ApprovalsRead => "approvals.read",
            OperationsRead => "operations.read",
            AgentsRun => "agents.run",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        use Scope::*;
        Some(match s {
            "crm.read" => CrmRead,
            "crm.write" => CrmWrite,
            "crm.propose" => CrmPropose,
            "crm.import" => CrmImport,
            "crm.export" => CrmExport,
            "research.propose" => ResearchPropose,
            "research.write" => ResearchWrite,
            "templates.read" => TemplatesRead,
            "templates.write" => TemplatesWrite,
            "mail.read" => MailRead,
            "mail.compose" => MailCompose,
            "mail.send" => MailSend,
            "mail.approve" => MailApprove,
            "mail.assign" => MailAssign,
            "campaign.compose" => CampaignCompose,
            "campaign.pause" => CampaignPause,
            "booking.read" => BookingRead,
            "booking.propose" => BookingPropose,
            "approvals.read" => ApprovalsRead,
            "operations.read" => OperationsRead,
            "agents.run" => AgentsRun,
            _ => return None,
        })
    }

    /// Scopes an agent principal may never hold (§5.4, §13.2).
    pub const AGENT_FORBIDDEN: &'static [Scope] = &[Scope::MailSend, Scope::MailApprove];
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSet(BTreeSet<Scope>);

impl ScopeSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Named differently from `FromIterator::from_iter` on purpose — clippy
    /// flags the confusion and a ScopeSet is not a general collection.
    pub fn of<I: IntoIterator<Item = Scope>>(it: I) -> Self {
        Self(it.into_iter().collect())
    }

    pub fn contains(&self, s: Scope) -> bool {
        self.0.contains(&s)
    }

    pub fn insert(&mut self, s: Scope) {
        self.0.insert(s);
    }

    pub fn iter(&self) -> impl Iterator<Item = Scope> + '_ {
        self.0.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Parse a scope string list (e.g. `service_credentials.scopes` TEXT[]).
    /// Unknown entries are dropped; callers that need strictness validate first.
    pub fn from_strings<I: IntoIterator<Item = S>, S: AsRef<str>>(it: I) -> Self {
        Self(
            it.into_iter()
                .filter_map(|s| Scope::parse(s.as_ref()))
                .collect(),
        )
    }

    pub fn to_strings(&self) -> Vec<String> {
        self.0.iter().map(|s| s.as_str().to_string()).collect()
    }
}

impl Serialize for ScopeSet {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_seq(self.0.iter().map(|s| s.as_str()))
    }
}

impl<'de> Deserialize<'de> for ScopeSet {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let v = Vec::<String>::deserialize(de)?;
        Ok(Self::from_strings(v.iter()))
    }
}

#[derive(Debug, Clone)]
pub enum AuthMethod {
    Session { session_id: Uuid },
    ServiceCredential { credential_id: Uuid },
    System { job_id: Uuid },
}

impl Ctx {
    /// System-acting context used by workers inside a claimed job.
    pub fn system(
        workspace_id: Uuid,
        principal_id: Uuid,
        job_id: Uuid,
        request_id: String,
    ) -> Self {
        Self {
            request_id,
            workspace_id,
            principal_id,
            principal_kind: PrincipalKind::System,
            role: Role::System,
            scopes: ScopeSet::new(),
            permission_version: 0,
            auth: AuthMethod::System { job_id },
            agent_run_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role × scope cell of §5.3 — the conjunctive formula relies on
    /// exactly these implied sets.
    #[test]
    fn role_implied_scopes_table() {
        use Scope::*;
        let all: &[(&[Scope], &[Scope])] = &[
            // (expected scopes, forbidden scopes)
            (
                &[
                    CrmRead,
                    CrmWrite,
                    CrmPropose,
                    CrmImport,
                    CrmExport,
                    ResearchPropose,
                    ResearchWrite,
                    TemplatesRead,
                    TemplatesWrite,
                    MailRead,
                    MailCompose,
                    MailSend,
                    MailApprove,
                    MailAssign,
                    CampaignCompose,
                    CampaignPause,
                    BookingRead,
                    BookingPropose,
                    ApprovalsRead,
                    OperationsRead,
                    AgentsRun,
                ],
                &[],
            ),
            (
                &[
                    CrmRead,
                    CrmWrite,
                    CrmPropose,
                    CrmImport,
                    CrmExport,
                    ResearchPropose,
                    ResearchWrite,
                    TemplatesRead,
                    TemplatesWrite,
                    MailRead,
                    MailCompose,
                    MailSend,
                    MailApprove,
                    MailAssign,
                    CampaignCompose,
                    CampaignPause,
                    BookingRead,
                    BookingPropose,
                    ApprovalsRead,
                    OperationsRead,
                    AgentsRun,
                ],
                &[],
            ),
            (
                &[
                    CrmRead,
                    CrmWrite,
                    CrmPropose,
                    TemplatesRead,
                    MailRead,
                    MailCompose,
                    MailSend,
                    CampaignCompose,
                    CampaignPause,
                    BookingRead,
                    BookingPropose,
                    ApprovalsRead,
                    OperationsRead,
                    ResearchPropose,
                ],
                &[
                    CrmImport,
                    CrmExport,
                    ResearchWrite,
                    TemplatesWrite,
                    MailApprove,
                    MailAssign,
                    AgentsRun,
                ],
            ),
        ];
        let roles = [Role::Admin, Role::Manager, Role::Member];
        for (role, (yes, no)) in roles.iter().zip(all.iter()) {
            let scopes = role.implied_scopes();
            for s in yes.iter() {
                assert!(scopes.contains(*s), "{role:?} missing {s:?}");
            }
            for s in no.iter() {
                assert!(!scopes.contains(*s), "{role:?} should not have {s:?}");
            }
        }
        // Agents/system carry explicit credential scopes only.
        for role in [Role::Agent, Role::System] {
            assert!(
                role.implied_scopes().is_empty(),
                "{role:?} has implied scopes"
            );
        }
    }
}
