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

impl Rights {
    fn bits(&self) -> [bool; 8] {
        [self.view, self.listen, self.draw, self.whisper, self.speak_client, self.control, self.admin, self.full_access]
    }

    /// Every right here is also in `other`.
    pub fn within(&self, other: &Rights) -> bool {
        self.bits().iter().zip(other.bits()).all(|(mine, theirs)| !*mine || theirs)
    }
}

fn state_rank(s: MemberState) -> u8 {
    match s {
        MemberState::Revoked => 0,
        MemberState::Pending => 1,
        MemberState::Approved => 2,
    }
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v = v.to_vec();
    v.sort();
    v.dedup();
    v
}

impl Member {
    /// Role, state, rights and a lead's groups: what the main PC has to confirm.
    fn same_grant(&self, other: &Member) -> bool {
        self.role == other.role && self.state == other.state && self.rights == other.rights && sorted(&self.lead_groups) == sorted(&other.lead_groups)
    }

    /// Nothing here gives more than `cur` did, so no confirmation is needed.
    pub fn grants_within(&self, cur: &Member) -> bool {
        if self.state == MemberState::Revoked {
            return true;
        }
        state_rank(self.state) <= state_rank(cur.state)
            && (self.role == Role::Agent || cur.role == Role::Lead)
            && self.rights.within(&cur.rights)
            && (cur.lead_groups.is_empty() || (!self.lead_groups.is_empty() && self.lead_groups.iter().all(|g| cur.lead_groups.contains(g))))
    }
}

/// What the main PC signs. The group is part of it, since a lead limited to some groups reaches
/// a PC through its group.
fn grant_message(team_id: &str, m: &Member) -> Vec<u8> {
    let bits: String = m.rights.bits().iter().map(|b| if *b { '1' } else { '0' }).collect();
    format!("tlsupporter-grant2:{team_id}:{}:{:?}:{:?}:{bits}:{}:{}:{}", m.id, m.role, m.state, sorted(&m.lead_groups).join(","), m.group, m.grant_ts).into_bytes()
}

fn grant_key_message(team_id: &str, key_b64: &str) -> Vec<u8> {
    format!("tlsupporter-grant-key:{team_id}:{key_b64}").into_bytes()
}

/// A change stamped this far ahead of the clock is refused: it would win over every later one.
const MAX_FUTURE_MS: i64 = 24 * 3600 * 1000;

fn from_the_future(rev: &Rev) -> bool {
    rev.ts > now_ms() + MAX_FUTURE_MS
}

/// Where a copy of the team data came from, so that the main PC can confirm a lead's change.
#[derive(Clone, Copy, Default)]
pub struct MergeFrom<'a> {
    /// This PC.
    pub me: &'a str,
    /// The lead whose copy this is, as proven by the connection.
    pub from: &'a str,
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
    /// A lead's signature that this PC's access was withdrawn. The service trusts only this.
    pub revoke_sig_b64: String,
    /// When the main PC confirmed role, state, rights and groups, and its signature over them.
    /// A newer confirmation replaces an older one, so an old one cannot be brought back.
    pub grant_ts: i64,
    pub grant_sig_b64: String,
    pub rev: Rev,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub deleted: bool,
    /// The leads turned the help button off for the employees of this group.
    pub help_hidden: bool,
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
    /// The main PC confirms roles and rights with its signature. Once set it stays set, and
    /// from then on a change that gives more needs that signature.
    pub owner_signs: bool,
    /// The key the main PC confirms rights with, kept in the profile of its Windows user, and
    /// the main PC's device signature over it. Set once. Until then the device key itself signs.
    pub grant_key_b64: String,
    pub grant_key_sig_b64: String,
}

fn merge_map<T: Clone>(mine: &mut BTreeMap<String, T>, other: &BTreeMap<String, T>, rev: impl Fn(&T) -> &Rev) -> bool {
    let mut changed = false;
    for (k, v) in other {
        if from_the_future(rev(v)) {
            continue;
        }
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
            Group { id: group_id, name: "Основна".into(), deleted: false, help_hidden: false, rev: Rev::new(&identity.id) },
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
        d.sign_grants(identity);
        Ok(d)
    }

    /// The main PC's device key, read from its own record and checked against its id.
    fn owner_key(&self) -> Option<Vec<u8>> {
        let m = self.members.get(&self.owner_id)?;
        let public = secure::unb64(&m.public_b64).ok()?;
        (secure::device_id(&public) == self.owner_id).then_some(public)
    }

    /// The separate key for rights, when the main PC has set one and signed it.
    fn checked_grant_key(&self, key_b64: &str, sig_b64: &str) -> Option<Vec<u8>> {
        let (device, key, sig) = (self.owner_key()?, secure::unb64(key_b64).ok()?, secure::unb64(sig_b64).ok()?);
        (key.len() == 32 && secure::verify(&device, &grant_key_message(&self.team_id, key_b64), &sig)).then_some(key)
    }

    /// The key that confirms rights: the separate one once it is set, the device key before.
    fn grant_key(&self) -> Option<Vec<u8>> {
        if self.grant_key_b64.is_empty() {
            self.owner_key()
        } else {
            self.checked_grant_key(&self.grant_key_b64, &self.grant_key_sig_b64)
        }
    }

    pub fn has_grant_key(&self) -> bool {
        !self.grant_key_b64.is_empty()
    }

    /// Run on the main PC once: from now on only this key confirms rights.
    pub fn set_grant_key(&mut self, identity: &secure::Identity, public: &[u8]) -> bool {
        if self.has_grant_key() || self.owner_id != identity.id || self.owner_key().as_deref() != Some(&identity.public[..]) {
            return false;
        }
        let key_b64 = secure::b64(public);
        let Ok(sig) = secure::sign(&identity.pkcs8, &grant_key_message(&self.team_id, &key_b64)) else { return false };
        self.grant_key_b64 = key_b64;
        self.grant_key_sig_b64 = secure::b64(&sig);
        true
    }

    /// The main PC confirmed this record's role, state, rights and group as they are now.
    pub fn grant_valid(&self, m: &Member) -> bool {
        let (Some(key), Ok(sig)) = (self.grant_key(), secure::unb64(&m.grant_sig_b64)) else { return false };
        secure::verify(&key, &grant_message(&self.team_id, m), &sig)
    }

    /// Shown to the leads: a change made here that the main PC has not confirmed yet.
    pub fn awaits_owner(&self, m: &Member) -> bool {
        self.owner_signs && !self.grant_valid(m)
    }

    /// Run on the main PC only: confirms every record it holds that is not confirmed yet.
    pub fn sign_grants(&mut self, identity: &secure::Identity) -> bool {
        self.sign_grants_with(identity, None)
    }

    /// `grant_private` is the separate key for rights. Without it only a team that has none yet
    /// is signed, with the device key.
    pub fn sign_grants_with(&mut self, identity: &secure::Identity, grant_private: Option<&[u8]>) -> bool {
        if self.owner_id != identity.id || self.owner_key().as_deref() != Some(&identity.public[..]) {
            return false;
        }
        let signer = if self.has_grant_key() {
            match grant_private {
                Some(p) if secure::public_of(p).is_some() && secure::public_of(p) == self.grant_key() => p.to_vec(),
                _ => return false,
            }
        } else {
            identity.pkcs8.clone()
        };
        let mut changed = !self.owner_signs;
        self.owner_signs = true;
        let unsigned: Vec<String> = self.members.values().filter(|m| !self.grant_valid(m)).map(|m| m.id.clone()).collect();
        for id in unsigned {
            let team_id = self.team_id.clone();
            let Some(m) = self.members.get_mut(&id) else { continue };
            let mut signed = m.clone();
            signed.grant_ts = now_ms().max(m.grant_ts + 1);
            let Ok(sig) = secure::sign(&signer, &grant_message(&team_id, &signed)) else { continue };
            signed.grant_sig_b64 = secure::b64(&sig);
            // Newer, so that it spreads, but still naming the lead who made the change: a PC
            // made a lead trusts that lead without asking.
            let author = if m.rev.by.is_empty() { identity.id.clone() } else { m.rev.by.clone() };
            signed.rev = Rev::new(&author);
            *m = signed;
            changed = true;
        }
        changed
    }

    /// The main PC takes a lead's change it was sent directly, when that lead may make it.
    /// It confirms the change with its signature right after.
    fn owner_takes(&self, v: &Member, cur: Option<&Member>, src: MergeFrom) -> bool {
        if src.me.is_empty() || src.me != self.owner_id || src.from.is_empty() || src.from == v.id || v.id == self.owner_id {
            return false;
        }
        let Some(author) = self.member(src.from).filter(|a| a.role == Role::Lead && a.state == MemberState::Approved) else { return false };
        // Only the main PC itself gives or takes the administration right.
        if v.rights.admin != cur.is_some_and(|c| c.rights.admin) {
            return false;
        }
        if author.rights.admin {
            return true;
        }
        // Without administration a lead manages only employees' PCs in their own groups.
        let employee = v.role == Role::Agent && cur.is_none_or(|c| c.role == Role::Agent && c.rights == v.rights && c.lead_groups == v.lead_groups);
        let reaches = |g: &String| author.lead_groups.is_empty() || author.lead_groups.contains(g);
        employee && reaches(&v.group) && cur.is_none_or(|c| reaches(&c.group))
    }

    /// An approved lead with the administration right.
    fn is_admin(&self, id: &str) -> bool {
        self.member(id).is_some_and(|a| a.role == Role::Lead && a.state == MemberState::Approved && a.rights.admin)
    }

    /// Whoever sent the change may take this much away. An empty author is a copy from this PC
    /// itself, not from the network.
    fn may_reduce(&self, author: &str, cur: &Member) -> bool {
        // The main PC's own record changes only with its signature, or nobody could confirm anything.
        if cur.id == self.owner_id {
            return false;
        }
        author.is_empty() || author == cur.id || cur.role == Role::Agent || self.is_admin(author)
    }

    /// Whoever sent the change may move this PC from its group into the new one.
    fn may_move(&self, author: &str, cur: &Member, to: &str) -> bool {
        if author.is_empty() || author == cur.id || self.is_admin(author) {
            return true;
        }
        self.member(author).is_some_and(|a| {
            let reaches = |g: &str| a.lead_groups.is_empty() || a.lead_groups.iter().any(|x| x == g);
            a.role == Role::Lead && a.state == MemberState::Approved && reaches(&cur.group) && reaches(to)
        })
    }

    fn merge_members(&mut self, other: &Directory, src: MergeFrom) -> bool {
        let guarded = self.owner_signs || other.owner_signs;
        let mut changed = false;
        for (k, v) in &other.members {
            if *k != v.id || from_the_future(&v.rev) {
                continue;
            }
            Rev::observe(v.rev.ts);
            let cur = self.members.get(k);
            if cur.is_some_and(|c| v.rev <= c.rev) {
                continue;
            }
            let plain = !guarded
                || match cur {
                    None => v.state != MemberState::Approved || self.grant_valid(v),
                    Some(c) => {
                        let confirmed = self.grant_valid(v) && v.grant_ts > c.grant_ts;
                        let rights = v.same_grant(c) || (v.grants_within(c) && self.may_reduce(src.from, c));
                        let place = v.group == c.group || self.may_move(src.from, c, &v.group);
                        confirmed || (rights && place)
                    }
                };
            let by_owner = !plain && self.owner_takes(v, cur, src);
            if !plain && !by_owner {
                continue;
            }
            let mut next = v.clone();
            if by_owner {
                // The author is the lead proven by the connection, whatever the record says.
                next.rev.by = src.from.to_string();
            }
            if let Some(c) = cur {
                if !self.grant_valid(&next) {
                    if next.same_grant(c) && next.group == c.group && self.grant_valid(c) {
                        // An older version passed the record on without the confirmation.
                        next.grant_ts = c.grant_ts;
                        next.grant_sig_b64 = c.grant_sig_b64.clone();
                    } else {
                        // An old confirmation must stay old, so it cannot be brought back later.
                        next.grant_ts = next.grant_ts.max(c.grant_ts);
                    }
                }
            }
            self.members.insert(k.clone(), next);
            changed = true;
        }
        changed
    }

    pub fn team_key(&self) -> Vec<u8> {
        secure::unb64(&self.team_key_b64).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.team_id.is_empty()
    }

    /// Merge another copy of the same team. Returns true when something changed.
    pub fn merge(&mut self, other: &Directory) -> bool {
        self.merge_from(other, MergeFrom::default())
    }

    pub fn merge_from(&mut self, other: &Directory, src: MergeFrom) -> bool {
        if self.team_id != other.team_id {
            return false;
        }
        // Decided before the records: a changed main PC would bring its own confirmations.
        let guarded = self.owner_signs || other.owner_signs;
        let mut changed = false;
        if !other.owner_id.is_empty() && other.owner_id != self.owner_id {
            // Set once. Before the main PC signs, PCs chosen while apart settle on the same one.
            if self.owner_id.is_empty() || (!guarded && other.owner_id < self.owner_id) {
                self.owner_id = other.owner_id.clone();
                changed = true;
            }
        }
        if other.owner_signs && !self.owner_signs {
            self.owner_signs = true;
            changed = true;
        }
        // Set once, and only as the main PC's device key signed it.
        if !self.has_grant_key() && self.checked_grant_key(&other.grant_key_b64, &other.grant_key_sig_b64).is_some() {
            self.grant_key_b64 = other.grant_key_b64.clone();
            self.grant_key_sig_b64 = other.grant_key_sig_b64.clone();
            changed = true;
        }
        changed |= self.merge_members(other, src);
        changed |= merge_map(&mut self.groups, &other.groups, |g| &g.rev);
        changed |= merge_map(&mut self.phrases, &other.phrases, |p| &p.rev);
        if !from_the_future(&other.policy.rev) {
            Rev::observe(other.policy.rev.ts);
            if other.policy.rev > self.policy.rev {
                self.policy = other.policy.clone();
                changed = true;
            }
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

    /// Withdraws a PC's access. Signed when this PC holds the team key, as every lead does.
    pub fn revoke_member(&mut self, id: &str, by: &str) {
        let sig = secure::unb64(&self.authority_private_b64).ok().and_then(|p| secure::sign(&p, &revoke_message(&self.team_id, id)).ok());
        if let Some(m) = self.members.get_mut(id) {
            m.state = MemberState::Revoked;
            m.revoke_sig_b64 = sig.map(|s| secure::b64(&s)).unwrap_or_default();
            m.rev = Rev::new(by);
        }
    }

    pub fn revocation_signed_by(&self, id: &str, authority_b64: &str) -> bool {
        let Some(m) = self.member(id).filter(|m| m.state == MemberState::Revoked) else { return false };
        let (Ok(authority), Ok(sig)) = (secure::unb64(authority_b64), secure::unb64(&m.revoke_sig_b64)) else { return false };
        secure::verify(&authority, &revoke_message(&self.team_id, id), &sig)
    }

    /// What a PC that lost its access is told: only its own record, nothing else about the team.
    pub fn revocation_notice(&self, id: &str) -> Directory {
        Directory {
            team_id: self.team_id.clone(),
            members: self.members.get(id).map(|m| (id.to_string(), m.clone())).into_iter().collect(),
            ..Default::default()
        }
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

fn revoke_message(team_id: &str, id: &str) -> Vec<u8> {
    format!("tlsupporter-revoke:{team_id}:{id}").into_bytes()
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
    /// The main PC, remembered once it confirms rights with its signature. From then on a lead
    /// gets secure screens only with rights that PC confirmed.
    pub owner_id: String,
}

/// Asks the service to let this PC leave its team. Sits in the data folder only for a moment.
pub const RESET_FILE: &str = "reset.req";
/// Sent instead of a password when a lead has withdrawn this PC's access.
pub const REVOKED_REQUEST: &str = "\u{1}revoked";

/// Whether the service may let this PC go: the admin password, or a signed withdrawal of access.
pub fn release_allowed(st: &LocalState, pin: &TeamPin, me: Option<&str>, request: &str) -> bool {
    if st.directory.team_id != pin.team_id {
        return false;
    }
    if request == REVOKED_REQUEST {
        return me.is_some_and(|id| st.directory.revocation_signed_by(id, &pin.authority_b64));
    }
    !request.is_empty() && st.directory.admin_password_signed_by(&pin.authority_b64) && secure::check_password(&st.directory.policy.admin_password, request)
}

fn team_pin_path() -> Option<PathBuf> {
    Some(crate::util::secure_dir()?.join("team.pin"))
}

pub fn load_team_pin() -> Option<TeamPin> {
    let text = std::fs::read_to_string(team_pin_path()?).ok()?;
    let mut lines = text.lines();
    let (team_id, authority_b64) = (lines.next()?.to_string(), lines.next()?.trim().to_string());
    let signed = lines.next() == Some("1");
    let released = lines.next() == Some("1");
    let owner_id = lines.next().unwrap_or_default().trim().to_string();
    Some(TeamPin { team_id, authority_b64, signed, released, owner_id })
}

pub fn save_team_pin(pin: &TeamPin) -> bool {
    let Some(path) = team_pin_path() else { return false };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let flag = |on: bool| if on { "1" } else { "0" };
    std::fs::write(path, format!("{}\n{}\n{}\n{}\n{}", pin.team_id, pin.authority_b64, flag(pin.signed), flag(pin.released), pin.owner_id)).is_ok()
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
    me: Option<String>,
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
    if gate.me.is_none() {
        gate.me = secure::Identity::load_existing().ok().map(|i| i.id);
    }
    let st = load_state();
    let genuine = release_allowed(&st, &pin, gate.me.as_deref(), &password);
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
    /// The PC the entry is about, so that a lead sees only the journal of their own groups.
    pub target_id: String,
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
    /// The console asks for its password as soon as the program opens.
    pub lock_on_start: bool,
    /// A session opens in a window of its own.
    pub session_window: bool,
    /// The employee sees rings where the lead clicks.
    pub show_taps: bool,
    /// The main PC looks for a new version by itself.
    pub auto_update: bool,
    /// The session shows the monitor where the employee's cursor is.
    pub follow_cursor: bool,
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
            lock_on_start: true,
            session_window: false,
            show_taps: true,
            auto_update: true,
            follow_cursor: false,
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
    let mut st: LocalState = load_protected(state_path());
    let d = &mut st.directory;
    if d.authority_private_b64.is_empty() && !d.team_id.is_empty() {
        if let Some(a) = load_authority().filter(|a| a.team_id == d.team_id) {
            d.authority_private_b64 = a.private_b64;
        }
    }
    st
}

/// The team's private key is kept apart from the rest, readable only by this Windows account:
/// `state.bin` is open to every user of the PC, as the system processes have to read it.
pub fn save_state(state: &LocalState) -> Result<()> {
    let d = &state.directory;
    if d.authority_private_b64.is_empty() {
        if d.is_empty() {
            remove_authority();
        }
        return save_protected(state_path(), state);
    }
    match store_authority(&d.team_id, &d.authority_private_b64) {
        Ok(()) => {
            let mut without = state.clone();
            without.directory.authority_private_b64.clear();
            save_protected(state_path(), &without)
        }
        Err(e) => {
            // Better kept where it was than lost.
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| crate::logi!("team key kept in the shared file: {e}"));
            save_protected(state_path(), state)
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct AuthorityFile {
    team_id: String,
    private_b64: String,
}

/// In the profile of the Windows account that runs the console. The system processes never
/// use these keys, so they get no path.
fn user_secret_path(name: &str) -> Option<PathBuf> {
    if crate::util::test_env("TLS_DATA_DIR").is_some() {
        return Some(data_dir().join(name));
    }
    if std::env::args().any(|a| a == "--service" || a == "--system-agent") {
        return None;
    }
    Some(local_app_data()?.join("TLSupporter").join(name))
}

fn local_app_data() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        path.map(PathBuf::from)
    }
}

const AUTHORITY_FILE: &str = "authority.bin";
/// The main PC's key for confirming rights.
const GRANT_FILE: &str = "grant.bin";

fn load_user_secret(name: &str) -> Option<AuthorityFile> {
    let bytes = std::fs::read(user_secret_path(name)?).ok()?;
    let plain = secure::dpapi_unprotect_user(&bytes).ok()?;
    serde_json::from_slice(&plain).ok()
}

fn store_user_secret(name: &str, team_id: &str, private_b64: &str) -> Result<()> {
    // Saving happens often, the keys rarely change.
    static STORED: parking_lot::Mutex<Vec<(String, String, String)>> = parking_lot::Mutex::new(Vec::new());
    let path = user_secret_path(name).ok_or_else(|| anyhow::anyhow!("no place for the key"))?;
    let wanted = (name.to_string(), team_id.to_string(), private_b64.to_string());
    if STORED.lock().contains(&wanted) && path.exists() {
        return Ok(());
    }
    let same = |a: &AuthorityFile| a.team_id == team_id && a.private_b64 == private_b64;
    if !load_user_secret(name).is_some_and(|a| same(&a)) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = AuthorityFile { team_id: team_id.to_string(), private_b64: private_b64.to_string() };
        let blob = secure::dpapi_protect_user(&serde_json::to_vec(&file)?)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, blob)?;
        std::fs::rename(tmp, &path)?;
        // Read back before any other copy of the key is dropped.
        if !load_user_secret(name).is_some_and(|a| same(&a)) {
            anyhow::bail!("key could not be read back");
        }
    }
    let mut stored = STORED.lock();
    stored.retain(|s| s.0 != name);
    stored.push(wanted);
    Ok(())
}

fn load_authority() -> Option<AuthorityFile> {
    load_user_secret(AUTHORITY_FILE)
}

fn store_authority(team_id: &str, private_b64: &str) -> Result<()> {
    store_user_secret(AUTHORITY_FILE, team_id, private_b64)
}

fn remove_authority() {
    for name in [AUTHORITY_FILE, GRANT_FILE] {
        if let Some(path) = user_secret_path(name) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The main PC's key for confirming rights in this team, when this Windows user holds it.
pub fn load_grant_private(team_id: &str) -> Option<Vec<u8>> {
    load_user_secret(GRANT_FILE).filter(|a| a.team_id == team_id).and_then(|a| secure::unb64(&a.private_b64).ok())
}

pub fn store_grant_private(team_id: &str, private: &[u8]) -> Result<()> {
    store_user_secret(GRANT_FILE, team_id, &secure::b64(private))
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
        let soon = now_ms() + 60_000;
        a.members.get_mut(&id).unwrap().rev = Rev { ts: soon, by: "a".into() };
        b.members.get_mut(&id).unwrap().name = "New".into();
        b.members.get_mut(&id).unwrap().rev = Rev { ts: soon + 1, by: "b".into() };
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
        let pin = TeamPin { team_id: d.team_id.clone(), authority_b64: d.authority_public_b64.clone(), signed: true, released: false, owner_id: String::new() };
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
        let pin = TeamPin { team_id: d.team_id.clone(), authority_b64: d.authority_public_b64.clone(), signed: false, released: false, owner_id: String::new() };
        let st = LocalState { directory: d, ..Default::default() };
        assert!(admin_password_accepted(&st, Some(&pin), "realpassword"));
        assert!(admin_password_accepted(&st, None, "realpassword"));
    }

    #[test]
    fn a_released_pc_can_join_another_team_but_keeps_checking_its_own() {
        let id = identity(1);
        let d = Directory::new("T", &id, "L", secure::hash_password("realpassword")).unwrap();
        let mut pin = TeamPin { team_id: "previous".into(), authority_b64: d.authority_public_b64.clone(), signed: true, released: false, owner_id: String::new() };
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
    fn only_a_withdrawal_signed_by_a_lead_releases_the_pc() {
        let id = identity(1);
        let mut d = Directory::new("T", &id, "L", secure::hash_password("realpassword")).unwrap();
        for pc in ["pc", "other"] {
            d.members.insert(pc.into(), Member { id: pc.into(), state: MemberState::Approved, ..Default::default() });
        }
        let pin = TeamPin { team_id: d.team_id.clone(), authority_b64: d.authority_public_b64.clone(), signed: true, released: false, owner_id: String::new() };
        let base = LocalState { directory: d.clone(), ..Default::default() };
        assert!(!release_allowed(&base, &pin, Some("pc"), REVOKED_REQUEST));

        let mut forged = base.clone();
        forged.directory.members.get_mut("pc").unwrap().state = MemberState::Revoked;
        assert!(!release_allowed(&forged, &pin, Some("pc"), REVOKED_REQUEST));

        let mut revoked = base.clone();
        revoked.directory.revoke_member("other", "lead");
        let sig = revoked.directory.member("other").unwrap().revoke_sig_b64.clone();
        assert!(!release_allowed(&revoked, &pin, Some("pc"), REVOKED_REQUEST));
        forged.directory.members.get_mut("pc").unwrap().revoke_sig_b64 = sig;
        assert!(!release_allowed(&forged, &pin, Some("pc"), REVOKED_REQUEST), "a signature for another PC does not count");

        revoked.directory.revoke_member("pc", "lead");
        assert!(release_allowed(&revoked, &pin, Some("pc"), REVOKED_REQUEST));
        assert!(!release_allowed(&revoked, &pin, None, REVOKED_REQUEST));

        let notice = revoked.directory.revocation_notice("pc");
        assert_eq!(notice.members.len(), 1);
        assert!(notice.policy.admin_password.is_empty() && notice.groups.is_empty());
        let mut stale = base.clone();
        assert!(stale.directory.merge(&notice));
        assert!(release_allowed(&stale, &pin, Some("pc"), REVOKED_REQUEST));
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

    fn real_identity() -> secure::Identity {
        let (pkcs8, public) = secure::generate_authority().unwrap();
        secure::Identity { id: secure::device_id(&public), pkcs8, public }
    }

    /// A team made by `owner` with an approved lead and an approved employee, all confirmed.
    fn signed_team() -> (secure::Identity, secure::Identity, secure::Identity, Directory) {
        let (owner, lead, agent) = (real_identity(), real_identity(), real_identity());
        let mut d = Directory::new("T", &owner, "Owner", String::new()).unwrap();
        for (who, role) in [(&lead, Role::Lead), (&agent, Role::Agent)] {
            d.members.insert(
                who.id.clone(),
                Member { id: who.id.clone(), public_b64: secure::b64(&who.public), role, state: MemberState::Approved, group: "g1".into(), rev: Rev::new(&owner.id), ..Default::default() },
            );
        }
        assert!(d.sign_grants(&owner));
        (owner, lead, agent, d)
    }

    fn later(m: &mut Member, by: &str) {
        m.rev = Rev::new(by);
    }

    #[test]
    fn a_lead_cannot_give_itself_more_rights_without_the_main_pc() {
        let (_owner, lead, _agent, d) = signed_team();
        assert!(d.grant_valid(&d.members[&lead.id]));
        let mut forged = d.clone();
        let m = forged.members.get_mut(&lead.id).unwrap();
        m.rights.admin = true;
        m.rights.full_access = true;
        later(m, &lead.id);
        let mut other = d.clone();
        assert!(!other.merge(&forged));
        assert!(!other.members[&lead.id].rights.admin);
        // Not even when the main PC receives it from that lead itself.
        let mut main = d.clone();
        let owner_id = main.owner_id.clone();
        assert!(!main.merge_from(&forged, MergeFrom { me: &owner_id, from: &lead.id }));
    }

    #[test]
    fn taking_rights_away_needs_no_confirmation_and_an_old_one_cannot_come_back() {
        let (_owner, lead, _agent, d) = signed_team();
        let signed_before = d.members[&lead.id].clone();
        let mut reduced = d.clone();
        let m = reduced.members.get_mut(&lead.id).unwrap();
        m.rights.control = false;
        later(m, "someone");
        let mut other = d.clone();
        assert!(other.merge(&reduced));
        assert!(!other.members[&lead.id].rights.control);
        // The old confirmed record, sent again with a newer stamp, does not restore the right.
        let mut replay = other.clone();
        let mut old = signed_before;
        later(&mut old, &lead.id);
        replay.members.insert(lead.id.clone(), old);
        assert!(!other.merge(&replay));
        assert!(!other.members[&lead.id].rights.control);
    }

    #[test]
    fn the_main_pc_confirms_a_change_an_administrating_lead_sent_it() {
        let (owner, lead, agent, mut d) = signed_team();
        d.members.get_mut(&lead.id).unwrap().rights.admin = true;
        assert!(d.sign_grants(&owner));
        let mut change = d.clone();
        let m = change.members.get_mut(&agent.id).unwrap();
        m.role = Role::Lead;
        later(m, &lead.id);
        // Another PC waits for the main PC.
        let mut other = d.clone();
        assert!(!other.merge_from(&change, MergeFrom { me: &agent.id, from: &lead.id }));
        // The main PC takes it from that lead and confirms it.
        let mut main = d.clone();
        assert!(main.merge_from(&change, MergeFrom { me: &owner.id, from: &lead.id }));
        assert!(main.awaits_owner(&main.members[&agent.id]));
        assert!(main.sign_grants(&owner));
        assert!(main.grant_valid(&main.members[&agent.id]));
        assert!(other.merge(&main));
        assert_eq!(other.members[&agent.id].role, Role::Lead);
    }

    #[test]
    fn only_the_main_pc_itself_gives_administration() {
        let (owner, lead, agent, mut d) = signed_team();
        d.members.get_mut(&lead.id).unwrap().rights.admin = true;
        assert!(d.sign_grants(&owner));
        let mut change = d.clone();
        let m = change.members.get_mut(&agent.id).unwrap();
        m.rights.admin = true;
        later(m, &lead.id);
        let mut main = d.clone();
        assert!(!main.merge_from(&change, MergeFrom { me: &owner.id, from: &lead.id }));
    }

    #[test]
    fn a_lead_without_administration_only_approves_employees_of_its_groups() {
        let (owner, lead, _agent, d) = signed_team();
        let newcomer = real_identity();
        let mut change = d.clone();
        let add = |c: &mut Directory, group: &str| {
            c.members.insert(
                newcomer.id.clone(),
                Member { id: newcomer.id.clone(), public_b64: secure::b64(&newcomer.public), state: MemberState::Approved, group: group.into(), rev: Rev::new(&lead.id), ..Default::default() },
            );
        };
        add(&mut change, "g1");
        let mut main = d.clone();
        main.members.get_mut(&lead.id).unwrap().lead_groups = vec!["g1".into()];
        let mut main2 = main.clone();
        assert!(main.merge_from(&change, MergeFrom { me: &owner.id, from: &lead.id }));
        let mut elsewhere = d.clone();
        add(&mut elsewhere, "g9");
        assert!(!main2.merge_from(&elsewhere, MergeFrom { me: &owner.id, from: &lead.id }));
    }

    #[test]
    fn a_confirmation_survives_an_older_version_passing_the_record_on() {
        let (_owner, lead, _agent, d) = signed_team();
        let mut stripped = d.clone();
        let m = stripped.members.get_mut(&lead.id).unwrap();
        m.grant_sig_b64.clear();
        m.grant_ts = 0;
        m.name = "Renamed".into();
        later(m, "old");
        let mut other = d.clone();
        assert!(other.merge(&stripped));
        assert_eq!(other.members[&lead.id].name, "Renamed");
        assert!(other.grant_valid(&other.members[&lead.id]));
    }

    #[test]
    fn a_change_from_far_in_the_future_is_refused_and_the_main_pc_stays() {
        let (_owner, lead, _agent, d) = signed_team();
        let mut future = d.clone();
        let m = future.members.get_mut(&lead.id).unwrap();
        m.name = "Future".into();
        m.rev = Rev { ts: i64::MAX, by: "x".into() };
        future.owner_id = "0000".into();
        let mut other = d.clone();
        assert!(!other.merge(&future));
        assert_ne!(other.members[&lead.id].name, "Future");
        assert_eq!(other.owner_id, d.owner_id);
        assert!(Rev::new("me").ts < i64::MAX / 2);
    }

    #[test]
    fn an_unconfirmed_new_pc_is_not_let_in() {
        let (_owner, lead, _agent, d) = signed_team();
        let stranger = real_identity();
        let mut change = d.clone();
        change.members.insert(
            stranger.id.clone(),
            Member { id: stranger.id.clone(), role: Role::Lead, state: MemberState::Approved, rights: Rights { admin: true, ..Default::default() }, rev: Rev::new(&lead.id), ..Default::default() },
        );
        let mut other = d.clone();
        assert!(!other.merge(&change));
        assert!(other.member(&stranger.id).is_none());
    }

    #[test]
    fn the_main_pc_cannot_be_revoked_without_its_own_signature() {
        let (owner, lead, _agent, d) = signed_team();
        let mut forged = d.clone();
        let m = forged.members.get_mut(&owner.id).unwrap();
        m.state = MemberState::Revoked;
        later(m, &lead.id);
        let mut main = d.clone();
        assert!(!main.merge_from(&forged, MergeFrom { me: &owner.id, from: &lead.id }));
        assert_eq!(main.members[&owner.id].state, MemberState::Approved);
        let mut other = d.clone();
        assert!(!other.merge_from(&forged, MergeFrom { me: &lead.id, from: &lead.id }));
    }

    #[test]
    fn a_plain_lead_cannot_revoke_an_administrator() {
        let (owner, lead, _agent, mut d) = signed_team();
        let admin = real_identity();
        d.members.insert(
            admin.id.clone(),
            Member { id: admin.id.clone(), public_b64: secure::b64(&admin.public), role: Role::Lead, state: MemberState::Approved, rights: Rights { admin: true, ..Default::default() }, rev: Rev::new(&owner.id), ..Default::default() },
        );
        assert!(d.sign_grants(&owner));
        let mut forged = d.clone();
        let m = forged.members.get_mut(&admin.id).unwrap();
        m.state = MemberState::Revoked;
        later(m, &lead.id);
        let mut other = d.clone();
        assert!(!other.merge_from(&forged, MergeFrom { me: "x", from: &lead.id }));
        assert_eq!(other.members[&admin.id].state, MemberState::Approved);
        // An administrator may.
        let mut by_admin = d.clone();
        assert!(by_admin.merge_from(&forged, MergeFrom { me: "x", from: &admin.id }));
        assert_eq!(by_admin.members[&admin.id].state, MemberState::Revoked);
    }

    #[test]
    fn a_lead_of_the_group_revokes_an_employee() {
        let (_owner, lead, agent, d) = signed_team();
        let mut change = d.clone();
        let m = change.members.get_mut(&agent.id).unwrap();
        m.state = MemberState::Revoked;
        later(m, &lead.id);
        let mut other = d.clone();
        assert!(other.merge_from(&change, MergeFrom { me: "x", from: &lead.id }));
        assert_eq!(other.members[&agent.id].state, MemberState::Revoked);
    }

    #[test]
    fn the_main_pc_changes_its_own_record() {
        let (owner, _lead, _agent, d) = signed_team();
        let mut main = d.clone();
        let m = main.members.get_mut(&owner.id).unwrap();
        m.rights.full_access = false;
        later(m, &owner.id);
        assert!(main.sign_grants(&owner));
        let mut other = d.clone();
        assert!(other.merge_from(&main, MergeFrom { me: "x", from: &owner.id }));
        assert!(!other.members[&owner.id].rights.full_access);
    }

    #[test]
    fn a_lead_limited_to_its_groups_cannot_pull_in_a_pc_of_another_group() {
        let (owner, lead, agent, mut d) = signed_team();
        d.members.get_mut(&lead.id).unwrap().lead_groups = vec!["g2".into()];
        assert!(d.sign_grants(&owner));
        let mut forged = d.clone();
        let m = forged.members.get_mut(&agent.id).unwrap();
        m.group = "g2".into();
        later(m, &lead.id);
        let mut other = d.clone();
        assert!(!other.merge_from(&forged, MergeFrom { me: "x", from: &lead.id }));
        assert_eq!(other.members[&agent.id].group, "g1");
        // Nor through the main PC.
        let mut main = d.clone();
        assert!(!main.merge_from(&forged, MergeFrom { me: &owner.id, from: &lead.id }));
        // Within its own groups it may.
        let mut inside = d.clone();
        inside.members.get_mut(&lead.id).unwrap().lead_groups = vec!["g1".into(), "g2".into()];
        assert!(inside.sign_grants(&owner));
        let mut moved = inside.clone();
        let m = moved.members.get_mut(&agent.id).unwrap();
        m.group = "g2".into();
        later(m, &lead.id);
        let mut other = inside.clone();
        assert!(other.merge_from(&moved, MergeFrom { me: "x", from: &lead.id }));
    }

    #[test]
    fn once_the_main_pc_has_its_key_for_rights_the_device_key_no_longer_confirms() {
        let (owner, lead, _agent, mut d) = signed_team();
        let (grant_private, grant_public) = secure::generate_authority().unwrap();
        assert!(d.set_grant_key(&owner, &grant_public));
        // Records signed with the device key are no longer confirmed.
        assert!(!d.grant_valid(&d.members[&lead.id]));
        assert!(!d.sign_grants(&owner), "the device key alone does not sign any more");
        assert!(d.sign_grants_with(&owner, Some(&grant_private)));
        assert!(d.grant_valid(&d.members[&lead.id]));
        // The key is set once: another one, even signed by the device key, is ignored.
        let mut other = d.clone();
        let mut rival = d.clone();
        rival.grant_key_b64.clear();
        let (_p2, public2) = secure::generate_authority().unwrap();
        assert!(rival.set_grant_key(&owner, &public2));
        other.merge(&rival);
        assert_eq!(other.grant_key_b64, d.grant_key_b64);
        // A PC that had no key yet takes it from the main PC's copy, and only the main PC sets one.
        let (owner2, lead2, _agent2, plain) = signed_team();
        let (p3, k3) = secure::generate_authority().unwrap();
        assert!(!plain.clone().set_grant_key(&lead2, &k3));
        let mut main2 = plain.clone();
        assert!(main2.set_grant_key(&owner2, &k3));
        assert!(main2.sign_grants_with(&owner2, Some(&p3)));
        let mut fresh = plain.clone();
        assert!(fresh.merge(&main2));
        assert_eq!(fresh.grant_key_b64, main2.grant_key_b64);
        assert!(fresh.grant_valid(&fresh.members[&lead2.id]));
    }

    #[test]
    fn old_data_loads_with_defaults() {
        let m: Member = serde_json::from_str(r#"{"id":"x","future_field":1}"#).unwrap();
        assert_eq!(m.id, "x");
        assert!(m.rights.draw);
        assert!(!m.rights.full_access, "full access must be granted explicitly");
    }

}
