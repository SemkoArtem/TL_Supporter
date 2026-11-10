//! Team data shared between PCs and the settings kept on each PC.
//! All structures tolerate unknown or missing fields so old and new versions can talk.

use crate::secure;
use crate::util::{data_dir, now_ms};
use anyhow::Result;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Last-writer-wins stamp.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rev {
    pub ts: i64,
    pub by: String,
}

impl Rev {
    pub fn new(by: &str) -> Self {
        Self { ts: now_ms(), by: by.to_string() }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Agent,
    Lead,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum MemberState {
    #[default]
    Pending,
    Approved,
    Revoked,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Rights {
    pub view: bool,
    pub listen: bool,
    pub draw: bool,
    pub whisper: bool,
    pub speak_client: bool,
    pub control: bool,
    pub admin: bool,
    /// Control on the lock screen, the sign-in screen and administrator prompts. Off until granted.
    pub full_access: bool,
}

impl Default for Rights {
    fn default() -> Self {
        Self { view: true, listen: true, draw: true, whisper: true, speak_client: true, control: true, admin: true, full_access: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Member {
    pub id: String,
    pub public_b64: String,
    pub name: String,
    pub role: Role,
    pub state: MemberState,
    pub group: String,
    pub rights: Rights,
    /// Groups a lead can work with. Empty means all groups.
    pub lead_groups: Vec<String>,
    pub rev: Rev,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub deleted: bool,
    pub rev: Rev,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct PhraseSet {
    pub id: String,
    pub name: String,
    pub phrases: Vec<String>,
    pub deleted: bool,
    pub rev: Rev,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Policy {
    pub admin_password: String,
    pub allow_hide_key: bool,
    pub hide_return_min: u32,
    pub stroke_fade_sec: u32,
    pub help_reasons: Vec<String>,
    pub sound_on_connect: bool,
    pub show_help_button: bool,
    pub update_repo: String,
    /// How long a new hint stays on the employee's screen. Never less than 10 seconds.
    pub hint_show_sec: u32,
    pub rev: Rev,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            admin_password: String::new(),
            allow_hide_key: true,
            hide_return_min: 60,
            stroke_fade_sec: 8,
            help_reasons: vec![
                "Складний клієнт".into(),
                "Не знаю відповіді".into(),
                "Проблема з програмою".into(),
            ],
            sound_on_connect: true,
            show_help_button: true,
            update_repo: String::new(),
            hint_show_sec: 10,
            rev: Rev::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Directory {
    pub team_id: String,
    pub team_name: String,
    pub team_key_b64: String,
    pub authority_public_b64: String,
    /// Only leads hold this. Removed from every copy sent to an agent.
    pub authority_private_b64: String,
    pub members: BTreeMap<String, Member>,
    pub groups: BTreeMap<String, Group>,
    pub phrases: BTreeMap<String, PhraseSet>,
    pub policy: Policy,
}

fn merge_map<T: Clone>(mine: &mut BTreeMap<String, T>, other: &BTreeMap<String, T>, rev: impl Fn(&T) -> &Rev) -> bool {
    let mut changed = false;
    for (k, v) in other {
        let replace = match mine.get(k) {
            Some(cur) => rev(v) > rev(cur),
            None => true,
        };
        if replace {
            mine.insert(k.clone(), v.clone());
            changed = true;
        }
    }
    changed
}

impl Directory {
    pub fn new(team_name: &str, identity: &secure::Identity, lead_name: &str, password_hash: String) -> Result<Self> {
        let (private, public) = secure::generate_authority()?;
        let mut d = Directory {
            team_id: secure::b64(&secure::random_bytes::<12>()),
            team_name: team_name.to_string(),
            team_key_b64: secure::b64(&secure::random_bytes::<32>()),
            authority_public_b64: secure::b64(&public),
            authority_private_b64: secure::b64(&private),
            ..Default::default()
        };
        d.policy.admin_password = password_hash;
        d.policy.rev = Rev::new(&identity.id);
        d.members.insert(
            identity.id.clone(),
            Member {
                id: identity.id.clone(),
                public_b64: secure::b64(&identity.public),
                name: lead_name.to_string(),
                role: Role::Lead,
                state: MemberState::Approved,
                rights: Rights { full_access: true, ..Default::default() },
                rev: Rev::new(&identity.id),
                ..Default::default()
            },
        );
        let group_id = secure::b64(&secure::random_bytes::<6>());
        d.groups.insert(
            group_id.clone(),
            Group { id: group_id, name: "Основна".into(), deleted: false, rev: Rev::new(&identity.id) },
        );
        let set_id = secure::b64(&secure::random_bytes::<6>());
        d.phrases.insert(
            set_id.clone(),
            PhraseSet {
                id: set_id,
                name: "Загальні".into(),
                phrases: vec![
                    "Уточни номер договору".into(),
                    "Запропонуй перезвонити".into(),
                    "Попроси почекати на лінії".into(),
                    "Перевір дані клієнта".into(),
                ],
                deleted: false,
                rev: Rev::new(&identity.id),
            },
        );
        Ok(d)
    }

    pub fn team_key(&self) -> Vec<u8> {
        secure::unb64(&self.team_key_b64).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.team_id.is_empty()
    }

    /// Merge another copy of the same team. Returns true when something changed.
    pub fn merge(&mut self, other: &Directory) -> bool {
        if self.team_id != other.team_id {
            return false;
        }
        let mut changed = merge_map(&mut self.members, &other.members, |m| &m.rev);
        changed |= merge_map(&mut self.groups, &other.groups, |g| &g.rev);
        changed |= merge_map(&mut self.phrases, &other.phrases, |p| &p.rev);
        if other.policy.rev > self.policy.rev {
            self.policy = other.policy.clone();
            changed = true;
        }
        if self.authority_private_b64.is_empty() && !other.authority_private_b64.is_empty() {
            self.authority_private_b64 = other.authority_private_b64.clone();
            changed = true;
        }
        changed
    }

    /// Copy that is safe to give to an agent.
    pub fn for_agent(&self) -> Directory {
        let mut d = self.clone();
        d.authority_private_b64.clear();
        d
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.members.get(id)
    }

    pub fn is_approved_lead(&self, id: &str) -> bool {
        self.member(id).map(|m| m.role == Role::Lead && m.state == MemberState::Approved).unwrap_or(false)
    }

    /// Can this lead work with this target PC?
    pub fn lead_can_reach(&self, lead: &str, target: &str) -> bool {
        let (Some(l), Some(t)) = (self.member(lead), self.member(target)) else { return false };
        if l.role != Role::Lead || l.state != MemberState::Approved || t.state != MemberState::Approved {
            return false;
        }
        if t.role == Role::Lead && t.group.is_empty() {
            return false;
        }
        l.lead_groups.is_empty() || l.lead_groups.contains(&t.group)
    }

    pub fn group_name(&self, id: &str) -> String {
        self.groups.get(id).filter(|g| !g.deleted).map(|g| g.name.clone()).unwrap_or_else(|| "Без групи".into())
    }

    /// Proof that the owner of `public` was made a lead by an existing lead.
    pub fn lead_proof(&self, public: &[u8]) -> Option<Vec<u8>> {
        let private = secure::unb64(&self.authority_private_b64).ok()?;
        if private.is_empty() {
            return None;
        }
        let msg = lead_proof_message(&self.team_id, public);
        secure::sign(&private, &msg).ok()
    }

    pub fn check_lead_proof(&self, public: &[u8], proof: &[u8]) -> bool {
        let Ok(authority) = secure::unb64(&self.authority_public_b64) else { return false };
        secure::verify(&authority, &lead_proof_message(&self.team_id, public), proof)
    }
}

fn lead_proof_message(team_id: &str, public: &[u8]) -> Vec<u8> {
    let mut msg = b"tlsupporter-lead:".to_vec();
    msg.extend_from_slice(team_id.as_bytes());
    msg.extend_from_slice(public);
    msg
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct LogEntry {
    pub id: String,
    pub ts: i64,
    pub lead: String,
    pub target: String,
    pub action: String,
}

/// Settings of one lead console.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LeadSettings {
    pub console_password: String,
    pub lock_enabled: bool,
    pub lock_idle_min: u32,
    pub lock_with_windows: bool,
    pub lock_on_sleep: bool,
    pub ptt_key: u32,
    pub mic_toggle_key: u32,
    pub grid_view: bool,
    pub mic_mode_client: bool,
    pub personal_phrases: Vec<String>,
    pub light_theme: bool,
    pub marker_width: f32,
    pub semi_width: f32,
    pub semi_alpha: f32,
    pub rect_width: f32,
    pub arrow_width: f32,
    pub pointer_size: f32,
    /// How loud this lead hears the employee's microphone and the computer sound.
    pub mix_agent: f32,
    pub mix_system: f32,
    /// How much the lead's own microphone is amplified.
    pub mic_gain: f32,
    /// Empty means the Windows default device.
    pub mic_device: String,
    pub speaker_device: String,
    pub do_not_disturb: bool,
    /// PCs whose help requests this lead does not want.
    pub muted_help: Vec<String>,
}

impl Default for LeadSettings {
    fn default() -> Self {
        Self {
            console_password: String::new(),
            lock_enabled: false,
            lock_idle_min: 10,
            lock_with_windows: true,
            lock_on_sleep: true,
            ptt_key: 0x75,        // F6
            mic_toggle_key: 0x76, // F7
            grid_view: false,
            mic_mode_client: false,
            personal_phrases: Vec::new(),
            light_theme: false,
            marker_width: 5.0,
            semi_width: 22.0,
            semi_alpha: 0.38,
            rect_width: 3.0,
            arrow_width: 4.0,
            pointer_size: 10.0,
            mix_agent: 1.0,
            mix_system: 1.0,
            mic_gain: 1.0,
            mic_device: String::new(),
            speaker_device: String::new(),
            do_not_disturb: false,
            muted_help: Vec::new(),
        }
    }
}

/// Everything stored on this PC.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LocalState {
    pub display_name: String,
    pub directory: Directory,
    pub lead: LeadSettings,
    pub agent_sound: Option<bool>,
    /// A lead's PC temporarily working as an employee's PC. The team role itself does not change.
    pub work_as_agent: bool,
    /// The group was assigned only for employee mode, so it is cleared on the way back.
    pub agent_group_auto: bool,
    /// Leads this lead's PC has allowed to connect. Ordinary employees' PCs do not ask.
    pub trusted_leads: Vec<String>,
}

impl LocalState {
    pub fn role(&self, id: &str) -> Option<Role> {
        self.directory.member(id).map(|m| m.role)
    }

    /// Should this PC open the lead console?
    pub fn console_mode(&self, id: &str) -> bool {
        self.role(id) == Some(Role::Lead) && !self.work_as_agent
    }
}

fn state_path() -> PathBuf {
    data_dir().join("state.bin")
}

fn audit_path() -> PathBuf {
    data_dir().join("audit.bin")
}

/// Forget the team, the journal and all settings. The PC keeps its own key.
pub fn factory_reset() -> Result<()> {
    save_state(&LocalState::default())?;
    let _ = std::fs::remove_file(audit_path());
    Ok(())
}

fn save_protected<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let blob = secure::dpapi_protect(&bytes)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, blob)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn load_protected<T: DeserializeOwned + Default>(path: PathBuf) -> T {
    std::fs::read(&path)
        .ok()
        .and_then(|b| secure::dpapi_unprotect(&b).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn load_state() -> LocalState {
    load_protected(state_path())
}

pub fn save_state(state: &LocalState) -> Result<()> {
    save_protected(state_path(), state)
}

pub fn load_audit() -> Vec<LogEntry> {
    load_protected(audit_path())
}

pub fn save_audit(log: &[LogEntry]) -> Result<()> {
    save_protected(audit_path(), &log.to_vec())
}

pub fn state_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(state_path()).and_then(|m| m.modified()).ok()
}

/// Merge audit entries and keep the newest 20000.
pub fn merge_audit(mine: &mut Vec<LogEntry>, other: &[LogEntry]) -> bool {
    let known: std::collections::HashSet<String> = mine.iter().map(|e| e.id.clone()).collect();
    let before = mine.len();
    mine.extend(other.iter().filter(|e| !known.contains(&e.id)).cloned());
    let changed = mine.len() != before;
    if changed {
        mine.sort_by_key(|e| e.ts);
        let excess = mine.len().saturating_sub(20_000);
        mine.drain(..excess);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn employee_mode_turns_off_console_but_keeps_lead_role() {
        let id = identity(1);
        let directory = Directory::new("T", &id, "L", String::new()).unwrap();
        let mut st = LocalState { directory, ..Default::default() };
        assert!(st.console_mode(&id.id));
        st.work_as_agent = true;
        assert!(!st.console_mode(&id.id));
        assert!(st.directory.is_approved_lead(&id.id));
    }

    fn identity(tag: u8) -> secure::Identity {
        let public = vec![tag; 32];
        secure::Identity { pkcs8: vec![], id: secure::device_id(&public), public }
    }

    #[test]
    fn newest_record_wins() {
        let lead = identity(1);
        let mut a = Directory::new("T", &lead, "Lead", String::new()).unwrap();
        let mut b = a.clone();
        let id = lead.id.clone();
        a.members.get_mut(&id).unwrap().name = "Old".into();
        a.members.get_mut(&id).unwrap().rev = Rev { ts: 10_i64.pow(14), by: "a".into() };
        b.members.get_mut(&id).unwrap().name = "New".into();
        b.members.get_mut(&id).unwrap().rev = Rev { ts: 10_i64.pow(14) + 1, by: "b".into() };
        assert!(a.merge(&b));
        assert_eq!(a.members[&id].name, "New");
        assert!(!a.merge(&b));
    }

    #[test]
    fn other_team_is_ignored() {
        let lead = identity(1);
        let mut a = Directory::new("A", &lead, "L", String::new()).unwrap();
        let b = Directory::new("B", &lead, "L", String::new()).unwrap();
        assert!(!a.merge(&b));
    }

    #[test]
    fn agent_copy_has_no_authority_and_proof_works() {
        let lead = identity(1);
        let d = Directory::new("T", &lead, "L", String::new()).unwrap();
        let copy = d.for_agent();
        assert!(copy.authority_private_b64.is_empty());
        assert!(copy.lead_proof(b"pk").is_none());
        let proof = d.lead_proof(b"pk").unwrap();
        assert!(copy.check_lead_proof(b"pk", &proof));
        assert!(!copy.check_lead_proof(b"other", &proof));
    }

    #[test]
    fn lead_only_reaches_allowed_groups() {
        let lead = identity(1);
        let mut d = Directory::new("T", &lead, "L", String::new()).unwrap();
        let agent = identity(2);
        d.members.insert(
            agent.id.clone(),
            Member { id: agent.id.clone(), state: MemberState::Approved, group: "g1".into(), ..Default::default() },
        );
        assert!(d.lead_can_reach(&lead.id, &agent.id));
        d.members.get_mut(&lead.id).unwrap().lead_groups = vec!["g2".into()];
        assert!(!d.lead_can_reach(&lead.id, &agent.id));
    }

    #[test]
    fn audit_merge_dedupes() {
        let e = LogEntry { id: "1".into(), ts: 1, ..Default::default() };
        let mut log = vec![e.clone()];
        assert!(!merge_audit(&mut log, &[e]));
        assert!(merge_audit(&mut log, &[LogEntry { id: "2".into(), ts: 2, ..Default::default() }]));
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn old_data_loads_with_defaults() {
        let m: Member = serde_json::from_str(r#"{"id":"x","future_field":1}"#).unwrap();
        assert_eq!(m.id, "x");
        assert!(m.rights.draw);
        assert!(!m.rights.full_access, "full access must be granted explicitly");
    }

}
