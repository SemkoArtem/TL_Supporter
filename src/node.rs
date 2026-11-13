//! Connections between PCs, discovery and team data sync. There is no central server:
//! lead PCs find the others, and every lead keeps a full copy of the team data.

use crate::agent_core::AgentInner;
use crate::capture::CaptureHub;
use crate::lead_core::LeadInner;
use crate::model::{self, Directory, LocalState, LogEntry, Member, MemberState, Rev, Role};
use crate::net::{self, Disc};
use crate::proto::{self, Hello, Msg, Role2, PROTOCOL_MIN, PROTOCOL_VERSION};
use crate::secure::{self, Identity};
use crate::util::{now_ms, APP_VERSION};
use anyhow::{anyhow, bail, Result};
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct NetConfig {
    pub quic_port: u16,
    pub discovery_port: u16,
    /// Ports of the system-level process that serves secure screens.
    pub secure_quic_port: u16,
    pub secure_discovery_port: u16,
    /// Extra discovery addresses, used for testing two copies on one PC.
    pub extra_peers: Vec<SocketAddr>,
}

/// The secure discovery port is always this far above the normal one.
pub const SECURE_DISCOVERY_OFFSET: u16 = 3;

impl NetConfig {
    pub fn from_env() -> Self {
        let port = |name: &str, default| crate::util::test_env(name).and_then(|v| v.parse().ok()).unwrap_or(default);
        let extra_peers = crate::util::test_env("TLS_PEERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        let quic_port = port("TLS_QUIC_PORT", proto::DEFAULT_QUIC_PORT);
        let discovery_port = port("TLS_DISCOVERY_PORT", proto::DEFAULT_DISCOVERY_PORT);
        Self {
            quic_port,
            discovery_port,
            secure_quic_port: quic_port + 1,
            secure_discovery_port: discovery_port + SECURE_DISCOVERY_OFFSET,
            extra_peers,
        }
    }
}

pub struct Peer {
    pub id: String,
    pub is_lead: bool,
    pub conn: quinn::Connection,
    pub app_version: String,
    tx: mpsc::UnboundedSender<Msg>,
}

impl Peer {
    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
    }
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub addr: SocketAddr,
    pub last_ms: i64,
}

/// The new name typed on an employee's PC, waiting for a lead to accept it.
pub const RENAME_FILE: &str = "rename.req";

pub struct JoinCode {
    pub code: String,
    pub role: Role,
    pub group: String,
    pub expires_ms: i64,
}

/// A code typed by a lead on the main screen. The new PC is first only found and shown,
/// and it joins after the lead has chosen a group.
#[derive(Clone, Default)]
pub struct AddCheck {
    pub code: String,
    pub expires_ms: i64,
    pub found_id: String,
    pub found_name: String,
    pub group: Option<String>,
    pub added: bool,
    pub dismissed: bool,
}

pub struct Node {
    pub identity: Identity,
    pub state: RwLock<LocalState>,
    pub audit: Mutex<Vec<LogEntry>>,
    pub endpoint: quinn::Endpoint,
    pub cfg: NetConfig,
    pub peers: Mutex<HashMap<String, Arc<Peer>>>,
    pub seen: Mutex<HashMap<String, Seen>>,
    pub dialing: Mutex<HashSet<String>>,
    pub join_codes: Mutex<Vec<JoinCode>>,
    pub add_check: Mutex<Option<AddCheck>>,
    /// What this process opened as: 0 not known, 1 employee's PC, 2 lead console.
    pub console_started: std::sync::atomic::AtomicU8,
    /// The journal has entries not yet written to disk.
    audit_dirty: std::sync::atomic::AtomicBool,
    /// This PC lost its access and is already leaving the team.
    leaving: std::sync::atomic::AtomicBool,
    /// PCs this lead has already given its newer version to during this run.
    offered: Mutex<HashSet<String>>,
    /// PCs that lost access and were told so during this run.
    told_revoked: Mutex<HashSet<String>>,
    pub secure_peers: Mutex<HashMap<String, Arc<Peer>>>,
    pub secure_dialing: Mutex<HashSet<String>>,
    pub hub: Arc<CaptureHub>,
    pub agent: Mutex<AgentInner>,
    pub lead: Mutex<LeadInner>,
    pub runtime: tokio::runtime::Handle,
    pub ui: crate::agent_ui::UiHandle,
    pub repaint: Mutex<Option<egui::Context>>,
}

pub fn request_repaint(node: &Node) {
    if let Some(ctx) = node.repaint.lock().as_ref() {
        ctx.request_repaint();
    }
}

impl Node {
    pub fn is_lead(&self) -> bool {
        self.state.read().directory.is_approved_lead(&self.identity.id)
    }

    /// This lead's PC works as an employee's PC for now.
    pub fn acting_as_agent(&self) -> bool {
        self.state.read().work_as_agent
    }

    /// A team made before the main PC existed: this PC becomes it, the only administrator.
    pub fn claim_main_pc(self: &Arc<Self>) {
        self.update_directory(|d, me| {
            if !d.owner_id.is_empty() {
                return;
            }
            let mine = me.to_string();
            d.owner_id = mine.clone();
            for m in d.members.values_mut() {
                let is_me = m.id == mine;
                if m.rights.admin != is_me {
                    m.rights.admin = is_me;
                    m.rev = Rev::new(&mine);
                }
            }
        });
        self.audit_log("—", "Вибрано головний ПК");
    }

    /// Make an employee's PC a lead, or a lead's PC an employee's again.
    pub fn set_member_role(self: &Arc<Self>, id: &str, role: Role) {
        if self.state.read().directory.owner_id == id {
            return;
        }
        let target = id.to_string();
        self.update_directory(|d, me| {
            let first_group = d.groups.values().find(|g| !g.deleted).map(|g| g.id.clone());
            if let Some(m) = d.members.get_mut(&target) {
                m.role = role;
                m.rev = Rev::new(me);
                // Either way these two are given separately afterwards.
                m.rights.admin = false;
                m.rights.full_access = false;
                // Without a group a lead's PC is hidden from the other leads.
                if role == Role::Lead && m.group.is_empty() {
                    m.group = first_group.unwrap_or_default();
                }
            }
        });
        let name = self.member_name(id);
        self.audit_log(&name, if role == Role::Lead { "Зроблено керівником" } else { "Зроблено працівником" });
    }

    /// Another lead changed this PC's role: reopen the program in the new role.
    fn follow_role_change(self: &Arc<Self>, sender: &str) {
        use std::sync::atomic::Ordering;
        {
            // Employee mode belongs to leads: without this a later promotion would not open the console.
            let mut st = self.state.write();
            if st.role(&self.identity.id) == Some(Role::Agent) && st.work_as_agent {
                st.work_as_agent = false;
                st.agent_group_auto = false;
            }
        }
        let started = self.console_started.load(Ordering::Relaxed);
        if started == 0 {
            return;
        }
        let console = self.state.read().console_mode(&self.identity.id);
        if console == (started == 2) {
            return;
        }
        {
            let mut st = self.state.write();
            if console {
                // Whoever promoted this PC does not have to ask for permission to connect.
                // The lead who made the change, even if another lead passed it on.
                let promoter = st.directory.member(&self.identity.id).map(|m| m.rev.by.clone()).filter(|b| !b.is_empty()).unwrap_or_else(|| sender.to_string());
                if !st.trusted_leads.iter().any(|l| *l == promoter) {
                    st.trusted_leads.push(promoter);
                }
            } else if !st.directory.is_approved_lead(&self.identity.id) {
                st.directory.authority_private_b64.clear();
            }
        }
        self.save();
        crate::logi!("role of this PC changed, reopening the program");
        crate::service::restart_program();
    }

    /// Asks the service to let this PC go, then resets it. Returns only when the service refused.
    pub fn leave_team(&self, password: &str) -> bool {
        if !model::request_team_release(password) {
            return false;
        }
        self.factory_reset()
    }

    pub fn factory_reset(&self) -> ! {
        // The leads take this PC off their lists.
        for p in self.peers.lock().values().filter(|p| p.is_lead) {
            p.send(Msg::LeftTeam);
        }
        std::thread::sleep(Duration::from_millis(600));
        // Cleared in memory too, so that no later save brings the old team back.
        *self.state.write() = LocalState::default();
        if let Err(e) = model::factory_reset() {
            crate::logi!("factory reset failed: {e}");
        }
        crate::service::restart_program()
    }

    /// Start looking for a new PC that shows this code.
    pub fn lead_check_code(&self, code: &str) {
        *self.add_check.lock() = Some(AddCheck { code: code.trim().to_string(), expires_ms: now_ms() + 30_000, ..Default::default() });
    }

    pub fn lead_check_view(&self) -> Option<AddCheck> {
        self.add_check.lock().clone().filter(|c| !c.dismissed)
    }

    /// The lead has seen which PC was found and chosen its group.
    pub fn lead_add_checked(&self, group: String) {
        if let Some(c) = self.add_check.lock().as_mut() {
            c.group = Some(group);
            c.expires_ms = now_ms() + 2 * 60_000;
        }
    }

    pub fn lead_check_cancel(&self) {
        let mut guard = self.add_check.lock();
        if guard.as_ref().is_some_and(|c| c.added) {
            // Stays active for a moment: the reply to the new PC may have been lost.
            if let Some(c) = guard.as_mut() {
                c.dismissed = true;
            }
        } else {
            *guard = None;
        }
    }

    /// A join request for the code typed on the main screen. `None` means another code,
    /// `Some(None)` means wait for the lead, otherwise the PC may join this group.
    fn held_join(self: &Arc<Self>, tag: &str, id: &str, public_b64: &str, name: &str, mac: &[u8]) -> Option<Option<(String, String)>> {
        let result = {
            let mut guard = self.add_check.lock();
            let c = guard.as_mut()?;
            if c.expires_ms < now_ms() || net::code_tag(&c.code) != tag || net::join_mac(&c.code, id, public_b64, name) != mac {
                return None;
            }
            if !c.found_id.is_empty() && c.found_id != id {
                return Some(None);
            }
            if c.found_id.is_empty() {
                c.found_id = id.to_string();
                c.expires_ms = now_ms() + 10 * 60_000;
            }
            c.found_name = name.to_string();
            c.group.clone().map(|g| (c.code.clone(), g))
        };
        request_repaint(self);
        Some(result)
    }

    pub fn switch_to_employee_mode(self: &Arc<Self>) -> ! {
        self.audit_log("—", "Перехід у режим працівника");
        self.flush_audit();
        self.state.write().work_as_agent = true;
        self.save();
        crate::service::restart_program()
    }

    /// Other leads see only PCs that have a group, so employee mode needs one.
    /// A group given just for this mode is taken away again when the lead returns.
    pub fn sync_employee_mode(self: &Arc<Self>) {
        if !self.is_lead() {
            return;
        }
        let (acting, auto, no_group, first) = {
            let st = self.state.read();
            let no_group = st.directory.member(&self.identity.id).is_some_and(|m| m.group.is_empty());
            let first = st.directory.groups.values().find(|g| !g.deleted).map(|g| g.id.clone());
            (st.work_as_agent, st.agent_group_auto, no_group, first)
        };
        let group = if acting && no_group {
            first
        } else if !acting && auto {
            Some(String::new())
        } else {
            None
        };
        let Some(group) = group else { return };
        self.update_directory(|d, me| {
            if let Some(m) = d.members.get_mut(me) {
                m.group = group;
                m.rev = Rev::new(me);
            }
        });
        self.state.write().agent_group_auto = acting;
        self.save();
    }

    pub fn my_name(&self) -> String {
        self.state.read().directory.member(&self.identity.id).map(|m| m.name.clone()).unwrap_or_default()
    }

    pub fn member_name(&self, id: &str) -> String {
        self.state.read().directory.member(id).map(|m| m.name.clone()).unwrap_or_else(|| id.to_string())
    }

    pub fn save(&self) {
        if let Err(e) = model::save_state(&self.state.read()) {
            crate::logi!("save failed: {e}");
        }
    }

    pub fn peer(&self, id: &str) -> Option<Arc<Peer>> {
        self.peers.lock().get(id).cloned()
    }

    /// Change team data, store it and send it to every connected PC.
    pub fn update_directory(self: &Arc<Self>, f: impl FnOnce(&mut Directory, &str)) {
        {
            let mut st = self.state.write();
            f(&mut st.directory, &self.identity.id);
        }
        self.save();
        self.broadcast_directory();
        self.on_directory_changed();
    }

    fn broadcast_directory(&self) {
        if !self.is_lead() {
            return;
        }
        let full = self.state.read().directory.clone();
        let agent_copy = full.for_agent();
        for p in self.peers.lock().values() {
            // Asked from the team data, so a PC just made a lead gets the lead's copy at once.
            let copy = if full.is_approved_lead(&p.id) {
                full.clone()
            } else if full.member(&p.id).is_some_and(|m| m.state == MemberState::Revoked) {
                full.revocation_notice(&p.id)
            } else {
                agent_copy.clone()
            };
            p.send(Msg::DirSync(Box::new(copy)));
        }
    }

    /// Drop anyone who lost access and apply new policy.
    pub fn on_directory_changed(self: &Arc<Self>) {
        let revoked: Vec<Arc<Peer>> = {
            let st = self.state.read();
            self.peers
                .lock()
                .values()
                .filter(|p| st.directory.member(&p.id).map(|m| m.state == MemberState::Revoked).unwrap_or(true))
                .cloned()
                .collect()
        };
        // A moment later, so that the PC first receives the news and can reset itself.
        for p in revoked {
            self.runtime.spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                p.conn.close(1u32.into(), b"revoked");
            });
        }
        self.agent_reconcile();
        self.ui.send(crate::agent_ui::UiCmd::Policy(self.state.read().directory.policy.clone()));
        request_repaint(self);
    }

    /// Write a journal entry. The journal exists only on lead PCs.
    pub fn audit_log(self: &Arc<Self>, target: &str, action: &str) {
        let entry = LogEntry {
            id: secure::b64(&secure::random_bytes::<9>()),
            ts: now_ms(),
            lead: self.my_name(),
            target: target.to_string(),
            action: action.chars().take(500).collect(),
            target_id: {
                let st = self.state.read();
                let mut same = st.directory.members.values().filter(|m| m.name == target);
                match (same.next(), same.next()) {
                    (Some(m), None) => m.id.clone(),
                    _ => String::new(),
                }
            },
        };
        {
            {
                let mut log = self.audit.lock();
                log.push(entry.clone());
                model::trim_journal(&mut log);
            }
            self.audit_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for p in self.peers.lock().values().filter(|p| p.is_lead) {
            p.send(Msg::LogSync(vec![entry.clone()]));
        }
    }

    /// A PC that was off during an update comes online with an older version: an administrating
    /// lead gives it the version it runs itself. The PC checks the release signature before installing.
    fn offer_update(self: &Arc<Self>, peer: &Arc<Peer>) {
        if !self.is_lead() || !crate::update::is_newer(APP_VERSION, &peer.app_version) {
            return;
        }
        if !self.state.read().directory.member(&self.identity.id).is_some_and(|m| m.rights.admin) {
            return;
        }
        let Some((exe, sig)) = crate::update::own_release() else { return };
        if !self.offered.lock().insert(peer.id.clone()) {
            return;
        }
        crate::logi!("giving version {APP_VERSION} to {} that runs {}", peer.id, peer.app_version);
        crate::update::send_update_to(self, peer.clone(), APP_VERSION, exe, sig);
    }

    pub fn forget_offer(&self, id: &str) {
        self.offered.lock().remove(id);
    }

    fn is_approved_lead_peer(&self, id: &str) -> bool {
        self.state.read().directory.is_approved_lead(id)
    }

    /// Withdraws a PC's access, signed so that its service lets it leave the team.
    pub fn revoke_member(self: &Arc<Self>, id: &str) {
        if self.state.read().directory.owner_id == id {
            return;
        }
        let target = id.to_string();
        self.update_directory(|d, me| d.revoke_member(&target, me));
    }

    /// This PC's access was withdrawn: it leaves the team and opens the first-run window again.
    pub fn follow_revocation(self: &Arc<Self>) {
        let revoked = self.state.read().directory.member(&self.identity.id).is_some_and(|m| m.state == MemberState::Revoked);
        if !revoked || self.leaving.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        crate::logi!("access to this PC was withdrawn, leaving the team");
        let node = self.clone();
        std::thread::spawn(move || {
            if !model::request_team_release(model::REVOKED_REQUEST) {
                crate::logi!("the service did not release this PC, full access will need --clear-secure-pin");
            }
            node.factory_reset();
        });
    }

    /// Signs the admin password if no lead has done it yet, as in teams from older versions.
    pub fn ensure_admin_signature(self: &Arc<Self>) {
        let needed = {
            let st = self.state.read();
            let d = &st.directory;
            d.is_approved_lead(&self.identity.id) && !d.authority_private_b64.is_empty() && !d.admin_password_signed_by(&d.authority_public_b64)
        };
        if needed {
            self.update_directory(|d, me| {
                d.sign_admin_password();
                d.policy.rev = Rev::new(me);
            });
        }
    }

    /// Writing the journal gets slower as it grows, so entries are saved together every 2 seconds.
    pub fn flush_audit(&self) {
        if self.audit_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let log = self.audit.lock().clone();
            let _ = model::save_audit(&log);
        }
    }

    fn make_hello(&self) -> Hello {
        let st = self.state.read();
        let role = st.role(&self.identity.id).unwrap_or(Role::Agent);
        let proof = if role == Role::Lead {
            st.directory.lead_proof(&self.identity.public).map(|p| secure::b64(&p)).unwrap_or_default()
        } else {
            String::new()
        };
        Hello {
            protocol: PROTOCOL_VERSION,
            app_version: APP_VERSION.to_string(),
            id: self.identity.id.clone(),
            name: st.directory.member(&self.identity.id).map(|m| m.name.clone()).unwrap_or_default(),
            role: Role2::from(role),
            lead_proof_b64: proof,
            team_id: st.directory.team_id.clone(),
            secure_channel: false,
        }
    }

    pub fn has_full_access(&self) -> bool {
        self.state.read().directory.member(&self.identity.id).map(|m| m.rights.full_access).unwrap_or(false)
    }

    pub fn secure_peer(&self, id: &str) -> Option<Arc<Peer>> {
        self.secure_peers.lock().get(id).cloned()
    }

    /// Reachable through the normal program or through the secure screen process.
    pub fn is_reachable(&self, id: &str) -> bool {
        self.peer(id).is_some() || self.secure_peer(id).is_some()
    }

    pub fn dial_secure(self: &Arc<Self>, id: String, addr: SocketAddr) {
        if self.secure_peers.lock().contains_key(&id) || !self.secure_dialing.lock().insert(id.clone()) {
            return;
        }
        let node = self.clone();
        tokio::spawn(async move {
            let result = async {
                let conn = node.endpoint.connect(addr, "tlsupporter")?.await?;
                node.secure_dialing.lock().remove(&id);
                node.clone().run_secure_link(conn, id.clone()).await
            }
            .await;
            node.secure_dialing.lock().remove(&id);
            if let Err(e) = result {
                crate::logi!("secure link to {id}: {e}");
            }
        });
    }

    async fn run_secure_link(self: Arc<Self>, conn: quinn::Connection, id: String) -> Result<()> {
        if !net::is_local_address(conn.remote_address().ip()) {
            bail!("non-local peer refused");
        }
        if secure::device_id(&net::peer_public_key(&conn)?) != id {
            bail!("secure peer key does not match");
        }
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&proto::encode(&Msg::Hello(self.make_hello()))?).await?;
        let hello = read_hello(&mut recv).await?;
        if hello.id != id || !hello.secure_channel {
            bail!("unexpected secure peer");
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        let peer = Arc::new(Peer { id: id.clone(), is_lead: false, conn: conn.clone(), app_version: hello.app_version, tx });
        self.secure_peers.lock().insert(id.clone(), peer.clone());
        crate::logi!("secure link up: {id}");
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let Ok(bytes) = proto::encode(&m) else { continue };
                if send.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let uni_node = self.clone();
        let uni_peer = peer.clone();
        let uni_conn = conn.clone();
        let unis = tokio::spawn(async move {
            while let Ok(mut stream) = uni_conn.accept_uni().await {
                let (n, p) = (uni_node.clone(), uni_peer.clone());
                tokio::spawn(async move {
                    let mut kind = [0u8; 1];
                    if stream.read_exact(&mut kind).await.is_ok() && kind[0] == proto::UNI_SCREEN {
                        if let Err(e) = n.lead_read_screen(p, stream, kind[0]).await {
                            crate::logi!("secure stream ended: {e}");
                        }
                    }
                });
            }
        });
        request_repaint(&self);
        let result = loop {
            match proto::read_msg(&mut recv).await {
                Ok(Some(m)) => self.lead_on_secure_message(&peer, m),
                Ok(None) => {}
                Err(e) => break e,
            }
        };
        unis.abort();
        {
            let mut peers = self.secure_peers.lock();
            if peers.get(&id).map(|p| Arc::ptr_eq(p, &peer)).unwrap_or(false) {
                peers.remove(&id);
            }
        }
        crate::logi!("secure link down: {id} ({result})");
        self.lead_secure_disconnected(&id);
        request_repaint(&self);
        Ok(())
    }

    /// Returns whether the peer is a lead, or an error when it must not connect.
    fn admit_peer(self: &Arc<Self>, hello: &Hello, public: &[u8], outgoing: bool) -> Result<bool> {
        if hello.protocol < PROTOCOL_MIN {
            bail!("peer protocol is too old");
        }
        if hello.id != secure::device_id(public) {
            bail!("peer id does not match its key");
        }
        let is_lead = {
            let st = self.state.read();
            if hello.team_id != st.directory.team_id {
                bail!("different team");
            }
            match st.directory.member(&hello.id) {
                Some(m) if m.state == MemberState::Revoked => bail!("access revoked"),
                Some(m) if m.role == Role::Lead && m.state == MemberState::Approved => true,
                other => {
                    let proven = hello.role == Role2::Lead
                        && secure::unb64(&hello.lead_proof_b64)
                            .map(|p| st.directory.check_lead_proof(public, &p))
                            .unwrap_or(false);
                    // A proof alone never makes anyone a lead, because whoever got hold of the team
                    // key could write one. It only lets a known PC receive its newer role.
                    if proven && other.is_some() {
                        true
                    } else if other.is_none() && outgoing {
                        bail!("unknown peer");
                    } else {
                        false
                    }
                }
            }
        };
        if !outgoing && !is_lead {
            bail!("only leads may open connections");
        }
        Ok(is_lead)
    }

    async fn run_connection(self: Arc<Self>, conn: quinn::Connection, outgoing: bool) -> Result<()> {
        if !net::is_local_address(conn.remote_address().ip()) {
            bail!("non-local peer refused");
        }
        let public = net::peer_public_key(&conn)?;
        let (mut send, mut recv) = if outgoing { conn.open_bi().await? } else { conn.accept_bi().await? };
        let mine = proto::encode(&Msg::Hello(self.make_hello()))?;
        let hello = if outgoing {
            send.write_all(&mine).await?;
            read_hello(&mut recv).await?
        } else {
            let h = read_hello(&mut recv).await?;
            send.write_all(&mine).await?;
            h
        };
        let revoked = self.is_lead() && hello.id == secure::device_id(&public) && self.state.read().directory.member(&hello.id).is_some_and(|m| m.state == MemberState::Revoked);
        if revoked {
            self.told_revoked.lock().insert(hello.id.clone());
            let notice = self.state.read().directory.revocation_notice(&hello.id);
            send.write_all(&proto::encode(&Msg::DirSync(Box::new(notice)))?).await?;
            let _ = send.finish();
            tokio::time::sleep(Duration::from_secs(2)).await;
            bail!("access revoked, the PC was told");
        }
        let is_lead = self.admit_peer(&hello, &public, outgoing)?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        let peer = Arc::new(Peer {
            id: hello.id.clone(),
            is_lead,
            conn: conn.clone(),
            app_version: hello.app_version.clone(),
            tx,
        });
        if let Some(old) = self.peers.lock().insert(peer.id.clone(), peer.clone()) {
            old.conn.close(0u32.into(), b"replaced");
        }
        crate::logi!("connected: {} lead={is_lead} v{}", peer.id, hello.app_version);

        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let Ok(bytes) = proto::encode(&m) else { continue };
                if send.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });

        if self.is_lead() {
            let d = self.state.read().directory.clone();
            peer.send(Msg::DirSync(Box::new(if is_lead { d } else { d.for_agent() })));
            if is_lead {
                peer.send(Msg::LogSync(self.audit.lock().clone()));
            }
        }
        if is_lead {
            self.agent_lead_connected(&peer);
        }
        if self.is_lead() {
            self.lead_target_connected(&peer);
        }
        self.offer_update(&peer);
        request_repaint(&self);

        let dgram_node = self.clone();
        let dgram_peer = peer.clone();
        let dgram_conn = conn.clone();
        let dgrams = tokio::spawn(async move {
            while let Ok(p) = dgram_conn.read_datagram().await {
                dgram_node.on_datagram(&dgram_peer, &p);
            }
        });
        let uni_node = self.clone();
        let uni_peer = peer.clone();
        let uni_conn = conn.clone();
        let unis = tokio::spawn(async move {
            while let Ok(stream) = uni_conn.accept_uni().await {
                let (n, p) = (uni_node.clone(), uni_peer.clone());
                tokio::spawn(async move {
                    // A picture stream simply ending is normal and is not written to the log.
                    if let Err(e) = n.on_uni_stream(p, stream).await {
                        if !e.to_string().contains("finished early") {
                            crate::logi!("stream ended: {e}");
                        }
                    }
                });
            }
        });

        let result = loop {
            match proto::read_msg(&mut recv).await {
                Ok(Some(m)) => self.on_message(&peer, m),
                Ok(None) => {} // a newer peer sent a message type this version does not know
                Err(e) => break e,
            }
        };
        dgrams.abort();
        unis.abort();
        {
            let mut peers = self.peers.lock();
            if peers.get(&peer.id).map(|p| Arc::ptr_eq(p, &peer)).unwrap_or(false) {
                peers.remove(&peer.id);
            }
        }
        crate::logi!("disconnected: {} ({result})", peer.id);
        self.agent_lead_disconnected(&peer);
        self.lead_target_disconnected(&peer);
        request_repaint(&self);
        Ok(())
    }

    fn on_message(self: &Arc<Self>, peer: &Arc<Peer>, m: Msg) {
        match m {
            Msg::Hello(_) => {}
            Msg::DirSync(d) => {
                // Asked from the team data each time: a PC let in only to learn its new role
                // or one that has just lost its role must not change anything here.
                if !peer.is_lead || !self.is_approved_lead_peer(&peer.id) {
                    return;
                }
                let changed = self.state.write().directory.merge(&d);
                if changed {
                    self.save();
                    self.broadcast_directory();
                    self.on_directory_changed();
                    self.follow_role_change(&peer.id);
                    self.follow_revocation();
                }
            }
            Msg::LogSync(entries) => {
                if !peer.is_lead || !self.is_lead() || !self.is_approved_lead_peer(&peer.id) {
                    return;
                }
                if model::merge_audit(&mut self.audit.lock(), &entries) {
                    self.audit_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            other => {
                if peer.is_lead {
                    self.agent_on_message(peer, other.clone());
                }
                if self.is_lead() {
                    self.lead_on_message(peer, other);
                }
            }
        }
    }

    fn on_datagram(self: &Arc<Self>, peer: &Arc<Peer>, p: &[u8]) {
        let Some((source, sender, seq, opus)) = proto::parse_audio(p) else { return };
        if source == proto::SRC_LEAD_VOICE && peer.is_lead && sender == secure::short_id(&peer.id) {
            self.agent_on_voice(peer, p, sender, seq, opus);
        }
        if self.is_lead() {
            self.lead_on_audio(peer, source, sender, seq, opus);
        }
    }

    async fn on_uni_stream(self: Arc<Self>, peer: Arc<Peer>, mut stream: quinn::RecvStream) -> Result<()> {
        let mut kind = [0u8; 1];
        stream.read_exact(&mut kind).await?;
        match kind[0] {
            proto::UNI_SCREEN | proto::UNI_THUMB if self.is_lead() => self.lead_read_screen(peer, stream, kind[0]).await,
            proto::UNI_UPDATE if peer.is_lead => crate::update::receive_update(stream).await,
            _ => Ok(()),
        }
    }

    pub fn dial(self: &Arc<Self>, id: String, addr: SocketAddr) {
        if self.peers.lock().contains_key(&id) || !self.dialing.lock().insert(id.clone()) {
            return;
        }
        let node = self.clone();
        tokio::spawn(async move {
            let result = async {
                let conn = node.endpoint.connect(addr, "tlsupporter")?.await?;
                node.dialing.lock().remove(&id);
                node.clone().run_connection(conn, true).await
            }
            .await;
            node.dialing.lock().remove(&id);
            if let Err(e) = result {
                crate::logi!("connection to {id} at {addr}: {e}");
            }
        });
    }

    async fn accept_loop(self: Arc<Self>) {
        while let Some(incoming) = self.endpoint.accept().await {
            if !net::is_local_address(incoming.remote_address().ip()) {
                incoming.refuse();
                continue;
            }
            let node = self.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        if let Err(e) = node.run_connection(conn, false).await {
                            crate::logi!("incoming connection: {e}");
                        }
                    }
                    Err(e) => crate::logi!("handshake failed: {e}"),
                }
            });
        }
    }

    fn should_dial(&self, id: &str) -> bool {
        let st = self.state.read();
        if id == self.identity.id {
            return false;
        }
        match st.directory.member(id) {
            None => false,
            // Once per run, to tell a PC that was off when it lost its access.
            Some(m) if m.state == MemberState::Revoked => self.is_lead() && !self.told_revoked.lock().contains(id),
            Some(m) if m.role == Role::Lead && m.state == MemberState::Approved => self.identity.id < m.id,
            // In employee mode this PC only keeps its link with other leads.
            Some(_) => !st.work_as_agent,
        }
    }

    async fn handle_discovery(self: &Arc<Self>, socket: &UdpSocket, packet: Disc, src: SocketAddr) {
        match packet {
            Disc::Probe { tag, id, port, mac } => {
                let (team_key, my_tag, name, proof) = {
                    let st = self.state.read();
                    let d = &st.directory;
                    let name = d.member(&self.identity.id).map(|m| m.name.clone()).unwrap_or_else(|| st.display_name.clone());
                    let proof = d.lead_proof(&self.identity.public).map(|p| secure::b64(&p)).unwrap_or_default();
                    (d.team_key(), net::team_tag(&d.team_id), name, proof)
                };
                if team_key.is_empty() || tag != my_tag || id == self.identity.id {
                    return;
                }
                if net::probe_mac(&team_key, &tag, &id, port) != mac {
                    return;
                }
                let public_b64 = secure::b64(&self.identity.public);
                let mac = net::pong_mac(&team_key, &tag, &self.identity.id, &name, &public_b64, self.cfg.quic_port, &proof);
                let reply = Disc::Pong {
                    tag,
                    id: self.identity.id.clone(),
                    name,
                    public_b64,
                    port: self.cfg.quic_port,
                    proof_b64: proof,
                    mac,
                };
                let _ = socket.send_to(&net::encode_disc(&reply), src).await;
            }
            Disc::Pong { tag, id, name, public_b64, port, proof_b64, mac } => {
                if !self.is_lead() || id == self.identity.id {
                    return;
                }
                let (team_key, my_tag) = {
                    let st = self.state.read();
                    (st.directory.team_key(), net::team_tag(&st.directory.team_id))
                };
                if tag != my_tag || net::pong_mac(&team_key, &tag, &id, &name, &public_b64, port, &proof_b64) != mac {
                    return;
                }
                let Ok(public) = secure::unb64(&public_b64) else { return };
                if secure::device_id(&public) != id {
                    return;
                }
                // New PCs join with a code. Answering a probe never adds anyone to the team.
                if self.state.read().directory.member(&id).is_none() {
                    return;
                }
                let addr = SocketAddr::new(src.ip(), port);
                self.seen.lock().insert(id.clone(), Seen { addr, last_ms: now_ms() });
                if self.should_dial(&id) {
                    self.dial(id, addr);
                }
                request_repaint(self);
            }
            Disc::JoinReq { code_tag, id, public_b64, name, mac } => self.handle_join(socket, code_tag, id, public_b64, name, mac, src).await,
            Disc::JoinOk { .. } => {}
            Disc::SecurePong { tag, id, port, mac } => {
                if !self.is_lead() || self.acting_as_agent() || !self.has_full_access() || id == self.identity.id {
                    return;
                }
                let (key, my_tag, reachable) = {
                    let st = self.state.read();
                    (st.directory.team_key(), net::team_tag(&st.directory.team_id), st.directory.lead_can_reach(&self.identity.id, &id))
                };
                if !reachable || tag != my_tag || net::secure_pong_mac(&key, &tag, &id, port) != mac {
                    return;
                }
                self.dial_secure(id, SocketAddr::new(src.ip(), port));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_join(self: &Arc<Self>, socket: &UdpSocket, tag: String, id: String, public_b64: String, name: String, mac: Vec<u8>, src: SocketAddr) {
        if !self.is_lead() {
            return;
        }
        let Ok(public) = secure::unb64(&public_b64) else { return };
        if secure::device_id(&public) != id {
            return;
        }
        let held = self.held_join(&tag, &id, &public_b64, &name, &mac);
        if matches!(held, Some(None)) {
            return;
        }
        let checked = held.is_some();
        let (code, role, group) = if let Some(Some((code, group))) = held {
            (code, Role::Agent, group)
        } else {
            let mut codes = self.join_codes.lock();
            codes.retain(|c| c.expires_ms > now_ms());
            let Some(pos) = codes.iter().position(|c| net::code_tag(&c.code) == tag) else { return };
            if net::join_mac(&codes[pos].code, &id, &public_b64, &name) != mac {
                return;
            }
            let c = codes.remove(pos); // one-time
            (c.code, c.role, c.group)
        };
        // A checked code answers repeated requests, because a UDP reply can get lost.
        let known = checked && self.state.read().directory.member(&id).is_some_and(|m| m.state == MemberState::Approved);
        // A PC already in the team that shows its code is moved into the chosen group and keeps its role.
        if known && self.state.read().directory.member(&id).is_some_and(|m| m.group != group) {
            let (mid, gr) = (id.clone(), group.clone());
            self.update_directory(|d, me| {
                if let Some(m) = d.members.get_mut(&mid) {
                    m.group = gr;
                    m.rev = Rev::new(me);
                }
            });
            self.audit_log(&name, "Переміщено в групу");
        }
        if !known {
            let (nid, pb, nm, gr) = (id.clone(), public_b64, name.clone(), group);
            self.update_directory(|d, me| {
                d.members.insert(
                    nid.clone(),
                    Member {
                        id: nid.clone(),
                        public_b64: pb,
                        name: nm,
                        role,
                        // The lead has already seen and accepted a checked PC.
                        state: if role == Role::Lead || checked { MemberState::Approved } else { MemberState::Pending },
                        group: gr,
                        rev: Rev::new(me),
                        ..Default::default()
                    },
                );
            });
            self.audit_log(&name, "Додано новий ПК за кодом");
        }
        if checked {
            if let Some(c) = self.add_check.lock().as_mut() {
                c.added = true;
            }
            request_repaint(self);
        }
        let dir = self.state.read().directory.clone();
        let send_dir = if role == Role::Lead { dir } else { dir.for_agent() };
        let Ok(json) = serde_json::to_vec(&send_dir) else { return };
        let Ok(sealed) = secure::seal_with_password(&net::normalize_code(&code), &json) else { return };
        let reply = Disc::JoinOk { code_tag: tag, id, sealed };
        let _ = socket.send_to(&net::encode_disc(&reply), src).await;
    }

    async fn discovery_loop(self: Arc<Self>, socket: Arc<UdpSocket>) {
        let mut buf = vec![0u8; 65535];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { continue };
            if !net::is_local_address(src.ip()) {
                continue;
            }
            if let Some(p) = net::decode_disc(&buf[..n]) {
                self.handle_discovery(&socket, p, src).await;
            }
        }
    }

    /// Only a running lead console sends probes, so idle agents stay silent.
    async fn probe_loop(self: Arc<Self>, socket: Arc<UdpSocket>) {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        let mut n = 0u32;
        loop {
            tick.tick().await;
            n += 1;
            if !self.is_lead() {
                continue;
            }
            // During a session looking for other PCs is less urgent, so it happens less often.
            let session = self.lead.lock().active.clone();
            if n % 5 != 0 && session.is_some_and(|id| self.peer(&id).is_some()) {
                continue;
            }
            let (key, tag) = {
                let st = self.state.read();
                (st.directory.team_key(), net::team_tag(&st.directory.team_id))
            };
            let mac = net::probe_mac(&key, &tag, &self.identity.id, self.cfg.quic_port);
            let bytes = net::encode_disc(&Disc::Probe { tag, id: self.identity.id.clone(), port: self.cfg.quic_port, mac });
            let mut targets = net::broadcast_targets(self.cfg.discovery_port);
            targets.extend(net::broadcast_targets(self.cfg.secure_discovery_port));
            targets.extend(self.cfg.extra_peers.iter().copied());
            targets.extend(self.cfg.extra_peers.iter().map(|p| SocketAddr::new(p.ip(), p.port() + SECURE_DISCOVERY_OFFSET)));
            for t in targets {
                let _ = socket.send_to(&bytes, t).await;
            }
        }
    }
}

async fn read_hello(recv: &mut quinn::RecvStream) -> Result<Hello> {
    proto::read_hello(recv).await
}

async fn bind_discovery(port: u16) -> Result<Arc<UdpSocket>> {
    let std_socket = std::net::UdpSocket::bind(("0.0.0.0", port))?;
    net::ignore_unreachable(&std_socket);
    std_socket.set_broadcast(true)?;
    std_socket.set_nonblocking(true)?;
    Ok(Arc::new(UdpSocket::from_std(std_socket)?))
}

/// Start networking on a new background runtime.
pub fn start(identity: Identity, state: LocalState, ui: crate::agent_ui::UiHandle) -> Result<Arc<Node>> {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let handle = runtime.handle().clone();
    let cfg = NetConfig::from_env();
    crate::audio::set_devices(&state.lead.mic_device, &state.lead.speaker_device);
    crate::audio::set_mic_gain(state.lead.mic_gain);
    crate::lead_core::set_mix(state.lead.mix_agent, state.lead.mix_system);
    let is_lead = state.directory.is_approved_lead(&identity.id);
    state.directory.observe_stamps();
    let node = handle.block_on(async {
        let endpoint = net::create_endpoint(&identity, cfg.quic_port)?;
        let socket = bind_discovery(cfg.discovery_port).await?;
        let node = Arc::new(Node {
            identity,
            state: RwLock::new(state),
            audit: Mutex::new(if is_lead { model::load_audit() } else { Vec::new() }),
            endpoint,
            cfg,
            peers: Mutex::new(HashMap::new()),
            seen: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashSet::new()),
            join_codes: Mutex::new(Vec::new()),
            add_check: Mutex::new(None),
            console_started: std::sync::atomic::AtomicU8::new(0),
            audit_dirty: std::sync::atomic::AtomicBool::new(false),
            leaving: std::sync::atomic::AtomicBool::new(false),
            offered: Mutex::new(HashSet::new()),
            told_revoked: Mutex::new(HashSet::new()),
            secure_peers: Mutex::new(HashMap::new()),
            secure_dialing: Mutex::new(HashSet::new()),
            hub: Arc::new(CaptureHub::new(false)),
            agent: Mutex::new(AgentInner::default()),
            lead: Mutex::new(LeadInner::default()),
            runtime: tokio::runtime::Handle::current(),
            ui,
            repaint: Mutex::new(None),
        });
        tokio::spawn(node.clone().accept_loop());
        tokio::spawn(node.clone().discovery_loop(socket.clone()));
        tokio::spawn(node.clone().probe_loop(socket));
        tokio::spawn(node.clone().agent_tick_loop());
        tokio::spawn(node.clone().lead_tick_loop());
        anyhow::Ok(node)
    })?;
    // The runtime lives as long as the process.
    std::mem::forget(runtime);
    Ok(node)
}

/// Offers this PC under a code for some minutes, so that a lead can move it into a group.
pub fn announce_code(identity: Identity, code: String, name: String, minutes: u64) {
    std::thread::spawn(move || {
        let until = std::time::Instant::now() + Duration::from_secs(minutes * 60);
        while std::time::Instant::now() < until {
            if try_join(&identity, &code, &name, 1).is_ok() {
                crate::logi!("join code accepted by a lead");
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    });
}

/// Used by the first-run window: ask a lead on the network to accept this PC by code.
pub fn join_with_code(identity: &Identity, code: &str, name: &str) -> Result<Directory> {
    try_join(identity, code, name, 8)
}

/// Each round takes about two seconds when no lead answers.
pub fn try_join(identity: &Identity, code: &str, name: &str, rounds: u32) -> Result<Directory> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
        net::ignore_unreachable(&socket);
        socket.set_broadcast(true)?;
        let cfg = NetConfig::from_env();
        let public_b64 = secure::b64(&identity.public);
        let tag = net::code_tag(code);
        let req = Disc::JoinReq {
            code_tag: tag.clone(),
            id: identity.id.clone(),
            public_b64: public_b64.clone(),
            name: name.to_string(),
            mac: net::join_mac(code, &identity.id, &public_b64, name),
        };
        let bytes = net::encode_disc(&req);
        let mut targets = net::broadcast_targets(cfg.discovery_port);
        targets.extend(cfg.extra_peers.iter().copied());
        let mut buf = vec![0u8; 65535];
        for _ in 0..rounds {
            for t in &targets {
                let _ = socket.send_to(&bytes, t).await;
            }
            match tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await {
                Ok(Ok((n, _))) => {
                    if let Some(Disc::JoinOk { code_tag, id, sealed }) = net::decode_disc(&buf[..n]) {
                        if code_tag == tag && id == identity.id {
                            // A false answer from someone on the network must not end the search.
                            let Ok(plain) = secure::open_with_password(&net::normalize_code(code), &sealed) else { continue };
                            return Ok(serde_json::from_slice(&plain)?);
                        }
                    }
                }
                _ => continue,
            }
        }
        Err(anyhow!("{}", crate::i18n::tr("Керівника з таким кодом не знайдено. Перевір код і чи відкрита програма керівника.")))
    })
}
