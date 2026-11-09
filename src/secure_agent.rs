//! System-level process for secure screens: the lock screen, the sign-in screen and
//! administrator prompts. It runs apart from the employee's own program and does only three
//! things: show the screen, apply input and press Ctrl+Alt+Del. Every lead must prove their
//! device key and hold the full access right, so nothing on the employee's PC can drive it.

use crate::capture::{self, CaptureHub, Profile};
use crate::model::{self, LocalState, Rights};
use crate::net::{self, Disc};
use crate::node::NetConfig;
use crate::proto::{self, Hello, InputEv, Mode, Msg, Role2, ScreenLevel, PROTOCOL_MIN, PROTOCOL_VERSION};
use crate::secure::{self, Identity};
use crate::sysutil;
use crate::util::APP_VERSION;
use anyhow::{anyhow, bail, Result};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

enum InputCmd {
    Event(InputEv, u8),
    Sas,
}

struct Link {
    conn: quinn::Connection,
    tx: mpsc::UnboundedSender<Msg>,
    mode: Mode,
    screen_task: Option<AbortHandle>,
}

struct Secure {
    identity: Identity,
    state: RwLock<LocalState>,
    hub: Arc<CaptureHub>,
    holder: Mutex<Option<String>>,
    links: Mutex<HashMap<String, Link>>,
    input: std_mpsc::Sender<InputCmd>,
    cfg: NetConfig,
}

impl Secure {
    /// Rights of a lead, only when they may use secure screens on this PC.
    fn allowed(&self, lead: &str) -> Option<Rights> {
        let st = self.state.read();
        if !st.directory.lead_can_reach(lead, &self.identity.id) {
            return None;
        }
        st.directory.member(lead).map(|m| m.rights.clone()).filter(|r| r.full_access)
    }

    fn broadcast_holder(&self) {
        let holder = self.holder.lock().clone().unwrap_or_default();
        for l in self.links.lock().values() {
            let _ = l.tx.send(Msg::ControlState { holder: holder.clone() });
        }
    }

    fn apply_mode(self: &Arc<Self>, lead: &str, conn: &quinn::Connection, mode: Mode, rights: &Rights) {
        let mut links = self.links.lock();
        let Some(link) = links.get_mut(lead) else { return };
        if link.mode == mode {
            return;
        }
        link.mode = mode;
        if let Some(t) = link.screen_task.take() {
            t.abort();
        }
        let level = if rights.view { mode.screen } else { ScreenLevel::Off };
        if let Some(profile) = Profile::from_level(level, mode.quality) {
            let (hub, conn) = (self.hub.clone(), conn.clone());
            let monitor = mode.monitor;
            link.screen_task = Some(
                tokio::spawn(async move {
                    if let Err(e) = crate::agent_core::stream_tiles(hub, conn, monitor, profile, proto::UNI_SCREEN).await {
                        crate::logi!("secure screen stream ended: {e}");
                    }
                })
                .abort_handle(),
            );
        }
    }

    fn on_message(self: &Arc<Self>, lead: &str, conn: &quinn::Connection, m: Msg) -> Result<()> {
        // Rights are checked on every message so that a revoked lead loses access immediately.
        let rights = self.allowed(lead).ok_or_else(|| anyhow!("full access withdrawn"))?;
        let monitor = self.links.lock().get(lead).map(|l| l.mode.monitor).unwrap_or(0);
        match m {
            Msg::SetMode(mode) => self.apply_mode(lead, conn, mode, &rights),
            Msg::ControlRequest(want) => {
                {
                    let mut holder = self.holder.lock();
                    if want && rights.control && holder.is_none() {
                        *holder = Some(lead.to_string());
                    } else if !want && holder.as_deref() == Some(lead) {
                        *holder = None;
                    }
                }
                self.broadcast_holder();
            }
            Msg::Input(ev) => {
                if self.holder.lock().as_deref() == Some(lead) {
                    let _ = self.input.send(InputCmd::Event(ev, monitor));
                }
            }
            Msg::SendSas => {
                if self.holder.lock().as_deref() == Some(lead) {
                    let _ = self.input.send(InputCmd::Sas);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn make_hello(&self) -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            app_version: APP_VERSION.to_string(),
            id: self.identity.id.clone(),
            name: String::new(),
            role: Role2::Agent,
            lead_proof_b64: String::new(),
            team_id: self.state.read().directory.team_id.clone(),
            secure_channel: true,
        }
    }

    async fn run_connection(self: Arc<Self>, conn: quinn::Connection) -> Result<()> {
        if !net::is_local_address(conn.remote_address().ip()) {
            bail!("non-local peer refused");
        }
        let public = net::peer_public_key(&conn)?;
        let lead = secure::device_id(&public);
        let (mut send, mut recv) = conn.accept_bi().await?;
        let hello = match proto::read_msg(&mut recv).await? {
            Some(Msg::Hello(h)) => h,
            _ => bail!("expected hello"),
        };
        if hello.protocol < PROTOCOL_MIN || hello.id != lead {
            bail!("bad hello");
        }
        if hello.team_id != self.state.read().directory.team_id {
            bail!("different team");
        }
        self.allowed(&lead).ok_or_else(|| anyhow!("lead has no full access"))?;
        send.write_all(&proto::encode(&Msg::Hello(self.make_hello()))?).await?;

        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        let _ = tx.send(Msg::ScreenInfo(capture::list_monitors()));
        self.links.lock().insert(lead.clone(), Link { conn: conn.clone(), tx, mode: Mode::default(), screen_task: None });
        self.broadcast_holder();
        crate::logi!("secure link from {lead}");
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let Ok(bytes) = proto::encode(&m) else { continue };
                if send.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let result: Result<()> = async {
            loop {
                if let Some(m) = proto::read_msg(&mut recv).await? {
                    self.on_message(&lead, &conn, m)?;
                }
            }
        }
        .await;
        writer.abort();
        if let Some(l) = self.links.lock().remove(&lead) {
            if let Some(t) = l.screen_task {
                t.abort();
            }
        }
        let released = {
            let mut holder = self.holder.lock();
            let mine = holder.as_deref() == Some(&lead);
            if mine {
                *holder = None;
            }
            mine
        };
        if released {
            self.broadcast_holder();
        }
        conn.close(1u32.into(), b"closed");
        result
    }

    async fn accept_loop(self: Arc<Self>, endpoint: quinn::Endpoint) {
        while let Some(incoming) = endpoint.accept().await {
            if !net::is_local_address(incoming.remote_address().ip()) {
                incoming.refuse();
                continue;
            }
            let me = self.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        if let Err(e) = me.run_connection(conn).await {
                            crate::logi!("secure connection: {e}");
                        }
                    }
                    Err(e) => crate::logi!("secure handshake failed: {e}"),
                }
            });
        }
    }

    async fn discovery_loop(self: Arc<Self>, socket: UdpSocket) {
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { continue };
            if !net::is_local_address(src.ip()) {
                continue;
            }
            let Some(Disc::Probe { tag, id, port, mac }) = net::decode_disc(&buf[..n]) else { continue };
            let (key, my_tag) = {
                let st = self.state.read();
                (st.directory.team_key(), net::team_tag(&st.directory.team_id))
            };
            if key.is_empty() || tag != my_tag || net::probe_mac(&key, &tag, &id, port) != mac {
                continue;
            }
            let reply = Disc::SecurePong {
                mac: net::secure_pong_mac(&key, &tag, &self.identity.id, self.cfg.secure_quic_port),
                tag,
                id: self.identity.id.clone(),
                port: self.cfg.secure_quic_port,
            };
            let _ = socket.send_to(&net::encode_disc(&reply), src).await;
        }
    }

    /// Pick up team changes made by the employee's program, such as a withdrawn right.
    async fn reload_loop(self: Arc<Self>) {
        let mut last = model::state_mtime();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = model::state_mtime();
            if now != last {
                last = now;
                *self.state.write() = model::load_state();
                let leads: Vec<String> = self.links.lock().keys().cloned().collect();
                for id in leads.iter().filter(|l| self.allowed(l).is_none()) {
                    if let Some(link) = self.links.lock().remove(id) {
                        if let Some(t) = link.screen_task {
                            t.abort();
                        }
                        // Closing also ends the connection task, which releases control.
                        link.conn.close(1u32.into(), b"full access withdrawn");
                    }
                }
            }
        }
    }
}

/// All input is applied from one thread that follows the input desktop.
fn input_thread(me: Arc<Secure>, rx: std_mpsc::Receiver<InputCmd>) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut last: Option<(u8, f32, f32)> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(66)) {
            Ok(cmd) => {
                sysutil::attach_input_desktop();
                match cmd {
                    InputCmd::Sas => sysutil::send_sas(),
                    InputCmd::Event(ev, monitor) => {
                        if let Some(rect) = capture::monitor_rects().get(monitor as usize) {
                            sysutil::inject(ev, *rect);
                        }
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if me.links.lock().values().all(|l| l.mode.screen == ScreenLevel::Off) {
            continue;
        }
        sysutil::attach_input_desktop();
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            continue;
        }
        for (i, r) in capture::monitor_rects().iter().enumerate() {
            if p.x >= r.0 && p.x < r.0 + r.2 && p.y >= r.1 && p.y < r.1 + r.3 {
                let pos = (i as u8, (p.x - r.0) as f32 / r.2 as f32, (p.y - r.1) as f32 / r.3 as f32);
                if last != Some(pos) {
                    last = Some(pos);
                    for l in me.links.lock().values() {
                        let _ = l.tx.send(Msg::Cursor { monitor: pos.0, x: pos.1, y: pos.2 });
                    }
                }
                break;
            }
        }
    }
}

/// Entry point of `--system-agent`. Never returns unless startup fails.
pub fn run(identity: Identity) -> Result<()> {
    let cfg = NetConfig::from_env();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    runtime.block_on(async move {
        let endpoint = net::create_endpoint(&identity, cfg.secure_quic_port)?;
        let bound = std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], cfg.secure_discovery_port)))?;
        bound.set_nonblocking(true)?;
        let socket = UdpSocket::from_std(bound)?;
        let (input, input_rx) = std_mpsc::channel();
        let me = Arc::new(Secure {
            identity,
            state: RwLock::new(model::load_state()),
            hub: Arc::new(CaptureHub::new(true)),
            holder: Mutex::new(None),
            links: Mutex::new(HashMap::new()),
            input,
            cfg,
        });
        let worker = me.clone();
        std::thread::spawn(move || input_thread(worker, input_rx));
        tokio::spawn(me.clone().accept_loop(endpoint));
        tokio::spawn(me.clone().discovery_loop(socket));
        crate::logi!("secure agent started");
        me.reload_loop().await;
        anyhow::Ok(())
    })
}
