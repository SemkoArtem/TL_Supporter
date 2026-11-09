//! What happens on the employee's PC when leads connect. Screen and sound are produced only
//! while a lead asks for them. A lock of Windows stops them at once.

use crate::agent_ui::UiCmd;
use crate::audio::{self, CaptureKind, Player};
use crate::capture::{self, Profile};
use crate::node::{request_repaint, Node, Peer};
use crate::proto::{self, DrawOp, LeadActivity, Mode, Msg, ScreenLevel};
use crate::secure;
use crate::sysutil;
use crate::util::{color_for, now_ms};
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::AbortHandle;

struct Link {
    peer: Arc<Peer>,
    mode: Mode,
    thumb: bool,
    screen_task: Option<(u8, ScreenLevel, AbortHandle)>,
    thumb_task: Option<AbortHandle>,
    last_voice: i64,
    last_draw: i64,
}

struct AudioRun {
    stop: Arc<AtomicBool>,
    targets: Arc<Mutex<Vec<quinn::Connection>>>,
}

#[derive(Default)]
pub struct AgentInner {
    links: HashMap<String, Link>,
    control: Option<String>,
    audio: Option<AudioRun>,
    player: Option<Player>,
    player_last: i64,
    locked: bool,
    was_active: bool,
    last_activity: Vec<LeadActivity>,
    help_id: u32,
    help_open: bool,
    clip_seq: u32,
    clip_applied: Option<String>,
    cursor: Option<(u8, f32, f32)>,
    voice_logged: bool,
}

fn screen_task(node: &Arc<Node>, conn: quinn::Connection, monitor: u8, profile: Profile, kind: u8) -> AbortHandle {
    let hub = node.hub.clone();
    tokio::spawn(async move {
        if let Err(e) = stream_tiles(hub, conn, monitor, profile, kind).await {
            crate::logi!("screen stream ended: {e}");
        }
    })
    .abort_handle()
}

pub async fn stream_tiles(hub: Arc<capture::CaptureHub>, conn: quinn::Connection, monitor: u8, profile: Profile, kind: u8) -> Result<()> {
    let sub = hub.subscribe(monitor, profile);
    let mut rx = sub.publisher.tx.subscribe();
    let mut out = conn.open_uni().await?;
    out.write_all(&[kind, monitor]).await?;
    let mut sent: Vec<u64> = Vec::new();
    let mut size = (0u32, 0u32);
    loop {
        let (w, h, tile, work) = {
            let snap = sub.publisher.snap.read();
            if snap.versions.len() != sent.len() {
                sent = vec![0; snap.versions.len()];
            }
            let work: Vec<(usize, u64, Arc<Vec<u8>>)> = snap
                .versions
                .iter()
                .enumerate()
                .filter(|(i, v)| **v != sent[*i] && snap.jpegs[*i].is_some())
                .map(|(i, v)| (i, *v, snap.jpegs[i].clone().unwrap()))
                .collect();
            (snap.width, snap.height, snap.tile, work)
        };
        if w != 0 && (w, h) != size {
            size = (w, h);
            let mut header = vec![proto::FRAME_SIZE];
            header.extend_from_slice(&(w as u16).to_le_bytes());
            header.extend_from_slice(&(h as u16).to_le_bytes());
            out.write_all(&header).await?;
        }
        if work.is_empty() {
            rx.changed().await?;
            continue;
        }
        let cols = w.div_ceil(tile.min(w)) as usize;
        for (i, version, jpeg) in work {
            let (x, y) = ((i % cols) as u32 * tile.min(w), (i / cols) as u32 * tile.min(h));
            let mut frame = Vec::with_capacity(jpeg.len() + 9);
            frame.push(proto::FRAME_TILE);
            frame.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
            frame.extend_from_slice(&(x as u16).to_le_bytes());
            frame.extend_from_slice(&(y as u16).to_le_bytes());
            frame.extend_from_slice(&jpeg);
            out.write_all(&frame).await?;
            sent[i] = version;
        }
    }
}

fn start_audio(targets: Arc<Mutex<Vec<quinn::Connection>>>, stop: Arc<AtomicBool>, me: u32) {
    for (kind, source) in [(CaptureKind::Microphone, proto::SRC_AGENT_MIC), (CaptureKind::ComputerSound, proto::SRC_AGENT_SYSTEM)] {
        let t = targets.clone();
        let seq = AtomicU16::new(0);
        audio::run_capture(kind, stop.clone(), move |opus| {
            let n = seq.fetch_add(1, Ordering::Relaxed);
            let packet = bytes_packet(source, me, n, opus);
            for c in t.lock().iter() {
                let _ = c.send_datagram(packet.clone());
            }
        });
    }
}

fn bytes_packet(source: u8, me: u32, seq: u16, opus: &[u8]) -> bytes::Bytes {
    bytes::Bytes::from(proto::audio_packet(source, me, seq, opus))
}

impl Node {
    fn rights_of(&self, lead: &str) -> Option<crate::model::Rights> {
        let st = self.state.read();
        if st.directory.lead_can_reach(lead, &self.identity.id) {
            st.directory.member(lead).map(|m| m.rights.clone())
        } else {
            None
        }
    }

    pub fn agent_lead_connected(self: &Arc<Self>, peer: &Arc<Peer>) {
        self.agent.lock().links.insert(
            peer.id.clone(),
            Link { peer: peer.clone(), mode: Mode::default(), thumb: false, screen_task: None, thumb_task: None, last_voice: 0, last_draw: 0 },
        );
        peer.send(Msg::ScreenInfo(capture::list_monitors()));
        peer.send(Msg::Status { locked: self.agent.lock().locked });
        self.ui.send(UiCmd::LeadsOnline(self.agent.lock().links.len()));
    }

    pub fn agent_lead_disconnected(self: &Arc<Self>, peer: &Arc<Peer>) {
        let count = {
            let mut a = self.agent.lock();
            let still_same = a.links.get(&peer.id).map(|l| Arc::ptr_eq(&l.peer, peer)).unwrap_or(false);
            if still_same {
                if let Some(l) = a.links.remove(&peer.id) {
                    if let Some(t) = l.screen_task {
                        t.2.abort();
                    }
                    if let Some(t) = l.thumb_task {
                        t.abort();
                    }
                }
                if a.control.as_deref() == Some(&peer.id) {
                    a.control = None;
                }
            }
            a.links.len()
        };
        self.ui.send(UiCmd::ClearLead(peer.id.clone()));
        self.ui.send(UiCmd::LeadsOnline(count));
        self.agent_reconcile();
    }

    /// Bring running capture in line with what the connected leads currently ask for.
    pub fn agent_reconcile(self: &Arc<Self>) {
        let sound_policy = self.state.read().directory.policy.sound_on_connect;
        let me = secure::short_id(&self.identity.id);
        let mut play = false;
        {
            let mut guard = self.agent.lock();
            let a = &mut *guard;
            let locked = a.locked;
            let mut listeners = Vec::new();
            let mut any_active = false;
            for l in a.links.values_mut() {
                let rights = {
                    let st = self.state.read();
                    if st.directory.lead_can_reach(&l.peer.id, &self.identity.id) {
                        st.directory.member(&l.peer.id).map(|m| m.rights.clone())
                    } else {
                        None
                    }
                };
                let (can_view, can_listen) = rights.map(|r| (r.view, r.listen)).unwrap_or((false, false));
                let level = if locked || !can_view { ScreenLevel::Off } else { l.mode.screen };
                let wanted = Profile::from_level(level).map(|p| (l.mode.monitor, level, p));
                let current = l.screen_task.as_ref().map(|t| (t.0, t.1));
                if wanted.map(|w| (w.0, w.1)) != current {
                    if let Some(t) = l.screen_task.take() {
                        t.2.abort();
                    }
                    if let Some((m, lv, p)) = wanted {
                        l.screen_task = Some((m, lv, screen_task(self, l.peer.conn.clone(), m, p, proto::UNI_SCREEN)));
                    }
                }
                let thumb_on = l.thumb && !locked && can_view;
                if thumb_on && l.thumb_task.is_none() {
                    l.thumb_task = Some(screen_task(self, l.peer.conn.clone(), 0, Profile::Thumb, proto::UNI_THUMB));
                } else if !thumb_on {
                    if let Some(t) = l.thumb_task.take() {
                        t.abort();
                    }
                }
                if l.mode.listen && !locked && can_listen {
                    listeners.push(l.peer.conn.clone());
                }
                any_active |= l.mode.listen || l.mode.screen != ScreenLevel::Off;
            }
            if listeners.is_empty() {
                if let Some(run) = a.audio.take() {
                    run.stop.store(true, Ordering::Relaxed);
                }
            } else {
                if a.audio.is_none() {
                    let run = AudioRun { stop: Arc::new(AtomicBool::new(false)), targets: Arc::new(Mutex::new(Vec::new())) };
                    start_audio(run.targets.clone(), run.stop.clone(), me);
                    a.audio = Some(run);
                }
                *a.audio.as_ref().unwrap().targets.lock() = listeners;
            }
            if any_active && !a.was_active && sound_policy {
                play = true;
            }
            a.was_active = any_active;
            self.ui.send(UiCmd::Active(any_active));
        }
        if play {
            sysutil::play_connect_sound();
        }
    }

    pub fn agent_on_message(self: &Arc<Self>, peer: &Arc<Peer>, m: Msg) {
        let Some(rights) = self.rights_of(&peer.id) else { return };
        match m {
            Msg::SetMode(mode) => {
                if let Some(l) = self.agent.lock().links.get_mut(&peer.id) {
                    l.mode = mode;
                }
                if mode.screen != ScreenLevel::Full {
                    self.release_control(&peer.id);
                }
                self.agent_reconcile();
            }
            Msg::SetThumb(on) => {
                if let Some(l) = self.agent.lock().links.get_mut(&peer.id) {
                    l.thumb = on;
                }
                self.agent_reconcile();
            }
            Msg::Draw { op, .. } if rights.draw => {
                let others = {
                    let mut a = self.agent.lock();
                    if let Some(l) = a.links.get_mut(&peer.id) {
                        l.last_draw = now_ms();
                    }
                    a.links.values().filter(|l| l.peer.id != peer.id).map(|l| l.peer.clone()).collect::<Vec<_>>()
                };
                let name = self.member_name(&peer.id);
                self.ui.send(UiCmd::Draw { lead: peer.id.clone(), name, color: color_for(&peer.id), op: op.clone() });
                for o in others {
                    o.send(Msg::Draw { from: peer.id.clone(), op: op.clone() });
                }
            }
            Msg::Hint { id, text } if rights.whisper || rights.draw => {
                self.ui.send(UiCmd::Hint { lead: peer.id.clone(), name: self.member_name(&peer.id), color: color_for(&peer.id), id, text });
            }
            Msg::HelpTaken { id, by, name } => {
                let others = {
                    let mut a = self.agent.lock();
                    if a.help_open && a.help_id == id {
                        a.help_open = false;
                    }
                    a.links.values().map(|l| l.peer.clone()).collect::<Vec<_>>()
                };
                self.ui.send(UiCmd::HelpTaken(name.clone()));
                for o in others {
                    o.send(Msg::HelpTaken { id, by: by.clone(), name: name.clone() });
                }
            }
            Msg::ControlRequest(want) => self.on_control_request(peer, want, rights.control),
            Msg::Input(ev) => {
                let monitor = {
                    let a = self.agent.lock();
                    if a.control.as_deref() != Some(&peer.id) {
                        return;
                    }
                    a.links.get(&peer.id).map(|l| l.mode.monitor).unwrap_or(0)
                };
                let rects = capture::monitor_rects();
                if let Some(rect) = rects.get(monitor as usize) {
                    sysutil::inject(ev, *rect);
                }
            }
            Msg::Clipboard(text) => {
                let full = self.agent.lock().links.get(&peer.id).map(|l| l.mode.screen == ScreenLevel::Full).unwrap_or(false);
                if full {
                    self.agent.lock().clip_applied = Some(text.clone());
                    sysutil::clipboard_set(&text);
                }
            }
            _ => {}
        }
    }

    fn release_control(self: &Arc<Self>, lead: &str) {
        let changed = {
            let mut a = self.agent.lock();
            if a.control.as_deref() == Some(lead) {
                a.control = None;
                true
            } else {
                false
            }
        };
        if changed {
            self.broadcast_control();
        }
    }

    fn on_control_request(self: &Arc<Self>, peer: &Arc<Peer>, want: bool, allowed: bool) {
        {
            let mut a = self.agent.lock();
            let full = a.links.get(&peer.id).map(|l| l.mode.screen == ScreenLevel::Full).unwrap_or(false);
            if want && allowed && full && a.control.is_none() {
                a.control = Some(peer.id.clone());
            } else if !want && a.control.as_deref() == Some(&peer.id) {
                a.control = None;
            }
        }
        self.broadcast_control();
    }

    fn broadcast_control(&self) {
        let a = self.agent.lock();
        let holder = a.control.clone().unwrap_or_default();
        for l in a.links.values() {
            l.peer.send(Msg::ControlState { holder: holder.clone() });
        }
    }

    pub fn agent_on_voice(self: &Arc<Self>, peer: &Arc<Peer>, raw: &[u8], sender: u32, seq: u16, opus: &[u8]) {
        let Some(rights) = self.rights_of(&peer.id) else { return };
        if !rights.whisper {
            return;
        }
        let relay: Vec<quinn::Connection> = {
            let mut a = self.agent.lock();
            if a.locked {
                return;
            }
            let now = now_ms();
            a.player_last = now;
            if let Some(l) = a.links.get_mut(&peer.id) {
                l.last_voice = now;
            }
            if a.player.is_none() {
                a.player = Some(Player::new());
            }
            if !a.voice_logged { a.voice_logged = true; crate::logi!("first voice packet from lead"); }
            a.player.as_ref().unwrap().push(sender as u64, 0.0, opus, seq);
            a.links.values().filter(|l| l.peer.id != peer.id && l.mode.listen).map(|l| l.peer.conn.clone()).collect()
        };
        // Other leads on this employee hear the hint too, so they do not talk over each other.
        for c in relay {
            let _ = c.send_datagram(bytes::Bytes::copy_from_slice(raw));
        }
    }

    pub fn agent_request_help(self: &Arc<Self>, reason: String) {
        let (id, leads) = {
            let mut a = self.agent.lock();
            a.help_id = a.help_id.wrapping_add(1);
            a.help_open = true;
            (a.help_id, a.links.values().map(|l| l.peer.clone()).collect::<Vec<_>>())
        };
        if leads.is_empty() {
            self.agent.lock().help_open = false;
            self.ui.send(UiCmd::HelpNoLeads);
            return;
        }
        for l in leads {
            l.send(Msg::HelpRequest { id, reason: reason.clone() });
        }
        self.ui.send(UiCmd::HelpSent);
    }

    pub fn agent_cancel_help(self: &Arc<Self>) {
        let (id, leads) = {
            let mut a = self.agent.lock();
            a.help_open = false;
            (a.help_id, a.links.values().map(|l| l.peer.clone()).collect::<Vec<_>>())
        };
        for l in leads {
            l.send(Msg::HelpCancel { id });
        }
    }

    pub fn agent_hint_closed(self: &Arc<Self>, lead: &str, id: u32) {
        if let Some(p) = self.peer(lead) {
            p.send(Msg::HintClosed { id });
        }
    }

    fn publish_activity(self: &Arc<Self>) {
        let now = now_ms();
        let (list, peers) = {
            let mut a = self.agent.lock();
            let control = a.control.clone();
            let mut list: Vec<LeadActivity> = a
                .links
                .values()
                .map(|l| LeadActivity {
                    id: l.peer.id.clone(),
                    name: self.member_name(&l.peer.id),
                    watching: l.mode.screen != ScreenLevel::Off,
                    listening: l.mode.listen,
                    drawing: now - l.last_draw < 4000,
                    speaking: now - l.last_voice < 500,
                    controlling: control.as_deref() == Some(&l.peer.id),
                })
                .collect();
            list.sort_by(|x, y| x.id.cmp(&y.id));
            if list == a.last_activity {
                return;
            }
            a.last_activity = list.clone();
            (list, a.links.values().map(|l| l.peer.clone()).collect::<Vec<_>>())
        };
        self.ui.send(UiCmd::Activity(list.clone()));
        for p in peers {
            p.send(Msg::Activity(list.clone()));
        }
    }

    fn poll_clipboard(self: &Arc<Self>) {
        let seq = sysutil::clipboard_sequence();
        let (changed, full_leads) = {
            let mut a = self.agent.lock();
            let changed = seq != a.clip_seq;
            a.clip_seq = seq;
            let leads: Vec<Arc<Peer>> =
                a.links.values().filter(|l| l.mode.screen == ScreenLevel::Full).map(|l| l.peer.clone()).collect();
            (changed, leads)
        };
        if !changed || full_leads.is_empty() {
            return;
        }
        let Some(text) = sysutil::clipboard_get() else { return };
        if self.agent.lock().clip_applied.take().as_deref() == Some(&text) {
            return;
        }
        if text.len() < 1 << 20 {
            for p in full_leads {
                p.send(Msg::Clipboard(text.clone()));
            }
        }
    }

    fn poll_cursor(self: &Arc<Self>) {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let viewers: Vec<Arc<Peer>> = {
            let a = self.agent.lock();
            a.links.values().filter(|l| l.mode.screen != ScreenLevel::Off).map(|l| l.peer.clone()).collect()
        };
        if viewers.is_empty() {
            return;
        }
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return;
        }
        for (i, r) in capture::monitor_rects().iter().enumerate() {
            if p.x >= r.0 && p.x < r.0 + r.2 && p.y >= r.1 && p.y < r.1 + r.3 {
                let pos = (i as u8, (p.x - r.0) as f32 / r.2 as f32, (p.y - r.1) as f32 / r.3 as f32);
                let mut a = self.agent.lock();
                if a.cursor != Some(pos) {
                    a.cursor = Some(pos);
                    drop(a);
                    for v in viewers {
                        v.send(Msg::Cursor { monitor: pos.0, x: pos.1, y: pos.2 });
                    }
                }
                break;
            }
        }
    }

    pub async fn agent_tick_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_millis(66));
        let mut n = 0u64;
        loop {
            tick.tick().await;
            n += 1;
            self.poll_cursor();
            if n % 4 == 0 {
                self.publish_activity();
            }
            if n % 8 == 0 {
                self.poll_clipboard();
            }
            if n % 15 == 0 {
                let test_locked = std::env::var("TLS_DATA_DIR").is_ok() && crate::util::data_dir().join("fake.lock").exists();
                let locked = sysutil::is_desktop_locked() || test_locked;
                let (changed, peers) = {
                    let mut a = self.agent.lock();
                    let changed = a.locked != locked;
                    a.locked = locked;
                    if now_ms() - a.player_last > 30_000 {
                        a.player = None;
                    }
                    (changed, a.links.values().map(|l| l.peer.clone()).collect::<Vec<_>>())
                };
                if changed {
                    for p in peers {
                        p.send(Msg::Status { locked });
                    }
                    self.ui.send(UiCmd::WindowsLocked(locked));
                    self.agent_reconcile();
                }
            }
            if n % 75 == 0 {
                request_repaint(&self);
            }
        }
    }

    /// Used by the overlay when the employee clears everything locally.
    pub fn agent_broadcast_clear(self: &Arc<Self>) {
        let peers: Vec<Arc<Peer>> = self.agent.lock().links.values().map(|l| l.peer.clone()).collect();
        for p in peers {
            p.send(Msg::Draw { from: self.identity.id.clone(), op: DrawOp::ClearAll });
        }
    }
}
