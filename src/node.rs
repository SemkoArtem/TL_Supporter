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
        let port = |name: &str, default| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
        let extra_peers = std::env::var("TLS_PEERS")
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

    /// Reopen this PC as an employee's PC. Returning needs the team admin password.
    pub fn factory_reset(&self) -> ! {
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
            let copy = if p.is_lead { full.clone() } else { agent_copy.clone() };
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
        for p in revoked {
            p.conn.close(1u32.into(), b"revoked");
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
            action: action.to_string(),
        };
        {
            let mut log = self.audit.lock();
            log.push(entry.clone());
            let _ = model::save_audit(&log);
        }
        for p in self.peers.lock().values().filter(|p| p.is_lead) {
            p.send(Msg::LogSync(vec![entry.clone()]));
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
        let mut added_lead = false;
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
                    if proven {
                        added_lead = true;
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
        if added_lead {
            let (id, public_b64, name) = (hello.id.clone(), secure::b64(public), hello.name.clone());
            self.update_directory(|d, me| {
                let keep = d.members.get(&id).cloned().unwrap_or_default();
                d.members.insert(
                    id.clone(),
                    Member {
                        id: id.clone(),
                        public_b64,
                        name: if keep.name.is_empty() { name } else { keep.name },
                        role: Role::Lead,
                        state: MemberState::Approved,
                        group: keep.group,
                        rights: keep.rights,
                        lead_groups: keep.lead_groups,
                        rev: Rev::new(me),
                    },
                );
            });
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
                    if let Err(e) = n.on_uni_stream(p, stream).await {
                        crate::logi!("stream ended: {e}");
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
                if !peer.is_lead {
                    return;
                }
                let changed = self.state.write().directory.merge(&d);
                if changed {
                    self.save();
                    self.broadcast_directory();
                    self.on_directory_changed();
                }
            }
            Msg::LogSync(entries) => {
                if !peer.is_lead || !self.is_lead() {
                    return;
                }
                let mut log = self.audit.lock();
                if model::merge_audit(&mut log, &entries) {
                    let _ = model::save_audit(&log);
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
            Some(m) if m.state == MemberState::Revoked => false,
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
                let known = self.state.read().directory.member(&id).is_some();
                if !known {
                    let proven = secure::unb64(&proof_b64)
                        .map(|p| self.state.read().directory.check_lead_proof(&public, &p))
                        .unwrap_or(false);
                    let (nid, pb) = (id.clone(), public_b64.clone());
                    self.update_directory(|d, me| {
                        d.members.insert(
                            nid.clone(),
                            Member {
                                id: nid.clone(),
                                public_b64: pb,
                                name: if name.is_empty() { nid.clone() } else { name },
                                role: if proven { Role::Lead } else { Role::Agent },
                                state: if proven { MemberState::Approved } else { MemberState::Pending },
                                rev: Rev::new(me),
                                ..Default::default()
                            },
                        );
                    });
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
        loop {
            tick.tick().await;
            if !self.is_lead() {
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
    match proto::read_msg(recv).await? {
        Some(Msg::Hello(h)) => Ok(h),
        _ => Err(anyhow!("expected hello")),
    }
}

async fn bind_discovery(port: u16) -> Result<Arc<UdpSocket>> {
    let std_socket = std::net::UdpSocket::bind(("0.0.0.0", port))?;
    std_socket.set_broadcast(true)?;
    std_socket.set_nonblocking(true)?;
    Ok(Arc::new(UdpSocket::from_std(std_socket)?))
}

/// Start networking on a new background runtime.
pub fn start(identity: Identity, state: LocalState, ui: crate::agent_ui::UiHandle) -> Result<Arc<Node>> {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(3).enable_all().build()?;
    let handle = runtime.handle().clone();
    let cfg = NetConfig::from_env();
    crate::audio::set_devices(&state.lead.mic_device, &state.lead.speaker_device);
    crate::audio::set_mic_gain(state.lead.mic_gain);
    let is_lead = state.directory.is_approved_lead(&identity.id);
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

/// Used by the first-run window: ask a lead on the network to accept this PC by code.
pub fn join_with_code(identity: &Identity, code: &str, name: &str) -> Result<Directory> {
    try_join(identity, code, name, 8)
}

/// Each round takes about two seconds when no lead answers.
pub fn try_join(identity: &Identity, code: &str, name: &str, rounds: u32) -> Result<Directory> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
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
                            let plain = secure::open_with_password(&net::normalize_code(code), &sealed)?;
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
