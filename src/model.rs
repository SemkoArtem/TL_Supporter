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

/// The newest stamp this PC has seen. A new change always goes after it, so a PC whose
/// clock runs behind does not lose its changes.
static NEWEST_STAMP: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

impl Rev {
    pub fn new(by: &str) -> Self {
        use std::sync::atomic::Ordering;
        let mut last = NEWEST_STAMP.load(Ordering::SeqCst);
        loop {
            let ts = (last + 1).max(now_ms());
            match NEWEST_STAMP.compare_exchange(last, ts, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Self { ts, by: by.to_string() },
                Err(current) => last = current,
            }
        }
    }

    pub fn observe(ts: i64) {
        NEWEST_STAMP.fetch_max(ts, std::sync::atomic::Ordering::SeqCst);
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
        Self { view: true, listen: true, draw: true, whisper: true, speak_client: true, control: true, admin: false, full_access: false }
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
    /// A lead's signature over the admin password hash, so that an employee cannot replace it.
    pub admin_sig_b64: String,
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
            admin_sig_b64: String::new(),
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
    /// The PC that made the team. Only it gives or takes the administration right.
    /// Empty in teams made before this existed, until an administrator chooses a PC.
    pub owner_id: String,
}

fn merge_map<T: Clone>(mine: &mut BTreeMap<String, T>, other: &BTreeMap<String, T>, rev: impl Fn(&T) -> &Rev) -> bool {
    let mut changed = false;
    for (k, v) in other {
        Rev::observe(rev(v).ts);
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
            owner_id: identity.id.clone(),
            ..Default::default()
        };
        d.policy.admin_password = password_hash;
        d.policy.rev = Rev::new(&identity.id);
        d.sign_admin_password();
        d.members.insert(
            identity.id.clone(),
            Member {
                id: identity.id.clone(),
                public_b64: secure::b64(&identity.public),
                name: lead_name.to_string(),
                role: Role::Lead,
                state: MemberState::Approved,
                rights: Rights { admin: true, full_access: true, ..Default::default() },
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
        Rev::observe(other.policy.rev.ts);
        if other.policy.rev > self.policy.rev {
            self.policy = other.policy.clone();
            changed = true;
        }
        // Set once. If two PCs were chosen while apart, every PC settles on the same one.
        if !other.owner_id.is_empty() && (self.owner_id.is_empty() || other.owner_id < self.owner_id) {
            self.owner_id = other.owner_id.clone();
            changed = true;
        }
        if self.authority_private_b64.is_empty() && !other.authority_private_b64.is_empty() {
            self.authority_private_b64 = other.authority_private_b64.clone();
            changed = true;
        }
        changed
    }

    /// Only leads can do this, because only they hold the team's private key.
    pub fn sign_admin_password(&mut self) {
        let Ok(private) = secure::unb64(&self.authority_private_b64) else { return };
        if let Ok(sig) = secure::sign(&private, &admin_message(&self.team_id, &self.policy.admin_password)) {
            self.policy.admin_sig_b64 = secure::b64(&sig);
        }
    }

    pub fn admin_password_signed_by(&self, authority_b64: &str) -> bool {
        let (Ok(authority), Ok(sig)) = (secure::unb64(authority_b64), secure::unb64(&self.policy.admin_sig_b64)) else { return false };
        secure::verify(&authority, &admin_message(&self.team_id, &self.policy.admin_password), &sig)
    }

    /// Stamps already stored here count as seen.
    pub fn observe_stamps(&self) {
        self.members.values().for_each(|m| Rev::observe(m.rev.ts));
        self.groups.values().for_each(|g| Rev::observe(g.rev.ts));
        self.phrases.values().for_each(|p| Rev::observe(p.rev.ts));
        Rev::observe(self.policy.rev.ts);
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
        verify_lead_proof(&self.authority_public_b64, &self.team_id, public, proof)
    }
}

fn admin_message(team_id: &str, hash: &str) -> Vec<u8> {
    format!("tlsupporter-admin:{team_id}:{hash}").into_bytes()
}

/// The team this PC belongs to, kept where ordinary users cannot write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamPin {
    pub team_id: String,
    pub authority_b64: String,
    /// A validly signed admin password has been seen, so an unsigned one no longer counts.
    pub signed: bool,
    /// Whoever knows the admin password has let this PC go, so the next team may replace this one.
    pub released: bool,
}

/// Asks the service to let this PC leave its team. Sits in the data folder only for a moment.
pub const RESET_FILE: &str = "reset.req";

fn team_pin_path() -> Option<PathBuf> {
    Some(crate::util::secure_dir()?.join("team.pin"))
}

pub fn load_team_pin() -> Option<TeamPin> {
    let text = std::fs::read_to_string(team_pin_path()?).ok()?;
    let mut lines = text.lines();
    let (team_id, authority_b64) = (lines.next()?.to_string(), lines.next()?.trim().to_string());
    let signed = lines.next() == Some("1");
    let released = lines.next() == Some("1");
    Some(TeamPin { team_id, authority_b64, signed, released })
}

pub fn save_team_pin(pin: &TeamPin) -> bool {
    let Some(path) = team_pin_path() else { return false };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let flag = |on: bool| if on { "1" } else { "0" };
    std::fs::write(path, format!("{}\n{}\n{}\n{}", pin.team_id, pin.authority_b64, flag(pin.signed), flag(pin.released))).is_ok()
}

pub fn remove_team_pin() {
    if let Some(path) = team_pin_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// The admin password check on an employee's PC. The data file can be edited by anyone signed
/// in there, so a password hash only counts when a lead of the remembered team signed it.
pub fn admin_password_accepted(st: &LocalState, pin: Option<&TeamPin>, password: &str) -> bool {
    // A team this PC was properly released from says nothing about the team it is in now.
    if let Some(pin) = pin.filter(|p| !(p.released && p.team_id != st.directory.team_id)) {
        let same_team = st.directory.team_id == pin.team_id;
        let signed = same_team && st.directory.admin_password_signed_by(&pin.authority_b64);
        if !same_team || (pin.signed && !signed) {
            return false;
        }
    }
    secure::check_password(&st.directory.policy.admin_password, password)
}

/// Called before a factory reset, while the team data still exists. The memory of the team is
/// out of reach of this program, so the service has to confirm the password and let the PC go.
pub fn request_team_release(password: &str) -> bool {
    let Some(pin) = load_team_pin() else { return true };
    if pin.released || load_state().directory.team_id != pin.team_id {
        return true;
    }
    let path = data_dir().join(RESET_FILE);
    let Ok(sealed) = secure::dpapi_protect(password.as_bytes()) else { return false };
    if std::fs::write(&path, sealed).is_err() {
        return false;
    }
    let mut released = false;
    for _ in 0..80 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if load_team_pin().is_some_and(|p| p.released) {
            released = true;
            break;
        }
    }
    let _ = std::fs::remove_file(path);
    released
}

#[derive(Default)]
pub struct ReleaseGate {
    seen: Option<std::time::SystemTime>,
    last_try: Option<std::time::Instant>,
}

/// Run by the service. Only reads the folder open to every user, trusts no password hash a lead
/// has not signed, and tries at most once in 3 seconds so that guessing stays slow.
pub fn process_team_release(gate: &mut ReleaseGate) {
    let path = data_dir().join(RESET_FILE);
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return };
    if !meta.is_file() || meta.len() > 4096 {
        return;
    }
    let stamp = meta.modified().ok();
    if stamp == gate.seen || gate.last_try.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3)) {
        return;
    }
    gate.seen = stamp;
    gate.last_try = Some(std::time::Instant::now());
    let Some(mut pin) = load_team_pin() else { return };
    if pin.released {
        return;
    }
    let Some(password) = std::fs::read(&path).ok().and_then(|b| secure::dpapi_unprotect(&b).ok()).and_then(|b| String::from_utf8(b).ok()) else { return };
    let st = load_state();
    let genuine = !password.is_empty()
        && st.directory.team_id == pin.team_id
        && st.directory.admin_password_signed_by(&pin.authority_b64)
        && secure::check_password(&st.directory.policy.admin_password, &password);
    if !genuine {
        crate::logi!("request to leave the team refused");
        return;
    }
    pin.released = true;
    if save_team_pin(&pin) {
        crate::logi!("this PC was released from its team");
    }
}

pub fn admin_password_ok(st: &LocalState, password: &str) -> bool {
    !password.is_empty() && admin_password_accepted(st, load_team_pin().as_ref(), password)
}

const MAX_JOURNAL: usize = 20_000;

/// Keeps the newest entries, otherwise the journal would outgrow what one message can carry.
pub fn trim_journal(log: &mut Vec<LogEntry>) {
    let excess = log.len().saturating_sub(MAX_JOURNAL);
    log.drain(..excess);
}

pub fn verify_lead_proof(authority_b64: &str, team_id: &str, public: &[u8], proof: &[u8]) -> bool {
    let Ok(authority) = secure::unb64(authority_b64) else { return false };
    secure::verify(&authority, &lead_proof_message(team_id, public), proof)
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
        trim_journal(mine);
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
    fn a_replaced_admin_password_is_refused() {
        let id = identity(1);
        let (private, public) = secure::generate_authority().unwrap();
        let mut d = Directory::new("T", &id, "L", secure::hash_password("realpassword")).unwrap();
        d.authority_private_b64 = secure::b64(&private);
        d.authority_public_b64 = secure::b64(&public);
        d.sign_admin_password();
        let pin = TeamPin { team_id: d.team_id.clone(), authority_b64: d.authority_public_b64.clone(), signed: true, released: false };
        let good = LocalState { directory: d.clone(), ..Default::default() };
        assert!(admin_password_accepted(&good, Some(&pin), "realpassword"));
        assert!(!admin_password_accepted(&good, Some(&pin), "wrong"));

        let mut forged = good.clone();
        forged.directory.policy.admin_password = secure::hash_password("attacker");
        assert!(!admin_password_accepted(&forged, Some(&pin), "attacker"));
        forged.directory.policy.admin_sig_b64.clear();
        assert!(!admin_password_accepted(&forged, Some(&pin), "attacker"));
        forged.directory.policy.admin_password.clear();
        assert!(!admin_password_accepted(&forged, Some(&pin), "anything"));

        let mut other_team = good.clone();
        other_team.directory.team_id = "other".into();
        assert!(!admin_password_accepted(&other_team, Some(&pin), "realpassword"));
    }

    #[test]
    fn a_team_not_yet_signed_keeps_working_until_a_signature_is_seen() {
        let id = identity(1);
        let mut d = Directory::new("T", &id, "L", secure::hash_password("realpassword")).unwrap();
        d.policy.admin_sig_b64.clear();
        let pin = TeamPin { team_id: d.team_id.clone(), authority_b64: d.authority_public_b64.clone(), signed: false, released: false };
        let st = LocalState { directory: d, ..Default::default() };
        assert!(admin_password_accepted(&st, Some(&pin), "realpassword"));
        assert!(admin_password_accepted(&st, None, "realpassword"));
    }

    #[test]
    fn a_released_pc_can_join_another_team_but_keeps_checking_its_own() {
        let id = identity(1);
        let d = Directory::new("T", &id, "L", secure::hash_password("realpassword")).unwrap();
        let mut pin = TeamPin { team_id: "previous".into(), authority_b64: d.authority_public_b64.clone(), signed: true, released: false };
        let st = LocalState { directory: d.clone(), ..Default::default() };
        assert!(!admin_password_accepted(&st, Some(&pin), "realpassword"));
        pin.released = true;
        assert!(admin_password_accepted(&st, Some(&pin), "realpassword"));
        assert!(!admin_password_accepted(&st, Some(&pin), "wrong"));

        pin.team_id = d.team_id.clone();
        let mut forged = st.clone();
        forged.directory.policy.admin_password = secure::hash_password("attacker");
        assert!(!admin_password_accepted(&forged, Some(&pin), "attacker"));
    }

    #[test]
    fn journal_keeps_only_the_newest_entries() {
        let mut log: Vec<LogEntry> = (0..20_005).map(|i| LogEntry { ts: i, ..Default::default() }).collect();
        trim_journal(&mut log);
        assert_eq!(log.len(), 20_000);
        assert_eq!(log[0].ts, 5);
    }

    #[test]
    fn a_change_goes_after_everything_already_seen() {
        let far = now_ms() + 3_600_000;
        Rev::observe(far);
        let a = Rev::new("a");
        let b = Rev::new("b");
        assert!(a.ts > far);
        assert!(b.ts > a.ts);
    }

    #[test]
    fn only_the_team_creator_starts_as_administrator() {
        let id = identity(1);
        let d = Directory::new("T", &id, "L", String::new()).unwrap();
        assert_eq!(d.owner_id, id.id);
        assert!(d.member(&id.id).unwrap().rights.admin);
        assert!(!Rights::default().admin);
    }

    #[test]
    fn main_pc_spreads_and_is_not_replaced_by_an_empty_copy() {
        let id = identity(1);
        let mut with = Directory::new("T", &id, "L", String::new()).unwrap();
        let mut without = with.clone();
        without.owner_id.clear();
        assert!(without.merge(&with));
        assert_eq!(without.owner_id, id.id);
        let mut empty = with.clone();
        empty.owner_id.clear();
        assert!(!with.merge(&empty));
        assert_eq!(with.owner_id, id.id);
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
