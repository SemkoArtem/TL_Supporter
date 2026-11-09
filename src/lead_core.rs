//! Lead side networking: per-employee view state, voice sending and help requests.

use crate::audio::{self, CaptureKind, Player};
use crate::node::{request_repaint, JoinCode, Node, Peer};
use crate::model::Role;
use crate::proto::{self, DrawOp, InputEv, LeadActivity, Mode, MonitorInfo, Msg, ScreenLevel, Tool};
use crate::secure;
use crate::sysutil;
use crate::util::{color_for, now_ms};
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Tile {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

#[derive(Default)]
pub struct ScreenBuf {
    pub width: u32,
    pub height: u32,
    pub epoch: u64,
    pub tiles: HashMap<(u32, u32), Tile>,
}

#[derive(Clone)]
pub struct Stroke {
    pub lead: String,
    pub id: u32,
    pub tool: Tool,
    pub monitor: u8,
    pub points: Vec<[f32; 2]>,
    pub ended_ms: Option<i64>,
    pub color: [u8; 3],
    pub stamps: Vec<i64>,
    pub width: f32,
    pub alpha: f32,
}

/// One hint of the conversation with an employee. Kept only in memory.
#[derive(Clone)]
pub struct HintEntry {
    pub id: u32,
    pub text: String,
    pub ts: i64,
    /// 0 unread, 1 read, 2 accepted, 3 rejected, 4 ignored.
    pub state: u8,
}

#[derive(Default)]
pub struct TargetView {
    pub monitors: Vec<MonitorInfo>,
    pub mode: Mode,
    pub thumb: Option<(usize, usize, Vec<u8>)>,
    pub thumb_rev: u64,
    pub screen: ScreenBuf,
    pub activity: Vec<LeadActivity>,
    pub locked: bool,
    pub cursor: Option<(u8, f32, f32)>,
    pub strokes: Vec<Stroke>,
    pub control_holder: String,
    pub hints: Vec<HintEntry>,
    pub consent: u8,
    pub last_audio_ms: i64,
    pub next_draw: u32,
    /// What the secure screen process was last asked to do.
    pub secure_mode: Mode,
    pub secure_holder: String,
    pub secure_control_sent: bool,
    pub want_control: bool,
}

#[derive(Clone)]
pub struct HelpItem {
    pub target: String,
    pub id: u32,
    pub reason: String,
    pub ts: i64,
    pub taken_by: String,
}

/// A request nobody took for this long counts as missed.
pub const HELP_MISSED_MS: i64 = 60_000;

impl HelpItem {
    pub fn is_missed(&self, now: i64) -> bool {
        self.taken_by.is_empty() && now - self.ts > HELP_MISSED_MS
    }
}

#[derive(Default)]
pub struct LeadInner {
    pub targets: HashMap<String, TargetView>,
    pub help: Vec<HelpItem>,
    pub active: Option<String>,
    pub thumbs_on: bool,
    pub console_locked: bool,
    pub player: Option<Player>,
    voice: Option<VoiceRun>,
    clip_seq: u32,
    clip_applied: Option<String>,
    next_hint: u32,
    pub help_alert: bool,
    audio_logged: u8,
}

struct VoiceRun {
    stop: Arc<AtomicBool>,
    target: Arc<Mutex<Option<quinn::Connection>>>,
}

pub static MIC_OPEN: AtomicBool = AtomicBool::new(false);
pub static MIC_LATCHED: AtomicBool = AtomicBool::new(false);
pub static MIC_BUTTON: AtomicBool = AtomicBool::new(false);
static VOICE_SENT_LOGGED: AtomicBool = AtomicBool::new(false);

fn decode_jpeg(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(data, options);
    let rgba = d.decode().ok()?;
    let info = d.info()?;
    Some((info.width as usize, info.height as usize, rgba))
}

impl Node {
    pub fn lead_target_connected(self: &Arc<Self>, peer: &Arc<Peer>) {
        let mut l = self.lead.lock();
        let thumbs = l.thumbs_on;
        l.targets.entry(peer.id.clone()).or_default();
        if thumbs {
            peer.send(Msg::SetThumb(true));
        }
    }

    pub fn lead_target_disconnected(self: &Arc<Self>, peer: &Arc<Peer>) {
        if self.peer(&peer.id).is_some() {
            return; // replaced by a newer connection
        }
        if self.secure_peer(&peer.id).is_some() {
            // The employee signed out, but the session continues on the sign-in screen.
            let mut l = self.lead.lock();
            if let Some(t) = l.targets.get_mut(&peer.id) {
                t.activity.clear();
                t.control_holder.clear();
            }
            return;
        }
        let mut l = self.lead.lock();
        if let Some(t) = l.targets.get_mut(&peer.id) {
            t.mode = Mode::default();
            t.screen = ScreenBuf { epoch: t.screen.epoch + 1, ..Default::default() };
            t.cursor = None;
            t.strokes.clear();
            t.activity.clear();
            t.control_holder.clear();
        }
    }

    pub fn lead_on_message(self: &Arc<Self>, peer: &Arc<Peer>, m: Msg) {
        let mut l = self.lead.lock();
        match m {
            Msg::ScreenInfo(mons) => l.targets.entry(peer.id.clone()).or_default().monitors = mons,
            Msg::Status { locked } => l.targets.entry(peer.id.clone()).or_default().locked = locked,
            Msg::Activity(a) => l.targets.entry(peer.id.clone()).or_default().activity = a,
            Msg::Cursor { monitor, x, y } => l.targets.entry(peer.id.clone()).or_default().cursor = Some((monitor, x, y)),
            Msg::ControlState { holder } => l.targets.entry(peer.id.clone()).or_default().control_holder = holder,
            Msg::Draw { from, op } => {
                let t = l.targets.entry(peer.id.clone()).or_default();
                apply_draw(t, &from, color_for(&from), op);
            }
            Msg::HintAck { id, state } => {
                if let Some(h) = l.targets.entry(peer.id.clone()).or_default().hints.iter_mut().find(|h| h.id == id) {
                    h.state = state;
                }
            }
            Msg::ConsentState { state } => l.targets.entry(peer.id.clone()).or_default().consent = state,
            Msg::HelpRequest { id, reason } => {
                {
                    let st = self.state.read();
                    if st.lead.do_not_disturb || st.lead.muted_help.iter().any(|m| m == &peer.id) {
                        return;
                    }
                }
                l.help.retain(|h| h.target != peer.id);
                l.help.insert(0, HelpItem { target: peer.id.clone(), id, reason, ts: now_ms(), taken_by: String::new() });
                l.help_alert = true;
                drop(l);
                sysutil::play_connect_sound();
                self.ui.send(crate::agent_ui::UiCmd::HelpIncoming(self.member_name(&peer.id)));
                self.restore_console();
                request_repaint(self);
                return;
            }
            Msg::HelpCancel { id } => l.help.retain(|h| !(h.target == peer.id && h.id == id)),
            Msg::HelpTaken { id, name, .. } => {
                if let Some(h) = l.help.iter_mut().find(|h| h.target == peer.id && h.id == id) {
                    h.taken_by = name;
                }
            }
            Msg::Clipboard(text) => {
                let active = l.active.as_deref() == Some(&peer.id);
                let controlling = l.targets.get(&peer.id).map(|t| t.control_holder == self.identity.id).unwrap_or(false);
                if active && controlling {
                    l.clip_applied = Some(text.clone());
                    sysutil::clipboard_set(&text);
                }
            }
            _ => return,
        }
        drop(l);
        request_repaint(self);
    }

    fn restore_console(&self) {
        crate::agent_ui::flash_console_window();
    }

    pub fn lead_on_audio(self: &Arc<Self>, peer: &Arc<Peer>, source: u8, sender: u32, seq: u16, opus: &[u8]) {
        let gain = {
            let st = self.state.read();
            match source {
                proto::SRC_AGENT_MIC => st.lead.mix_agent,
                proto::SRC_AGENT_SYSTEM => st.lead.mix_system,
                _ => 1.0,
            }
        };
        let mut l = self.lead.lock();
        if l.console_locked {
            return;
        }
        if !l.targets.get(&peer.id).map(|t| t.mode.listen).unwrap_or(false) {
            return;
        }
        if let Some(t) = l.targets.get_mut(&peer.id) {
            t.last_audio_ms = now_ms();
        }
        if l.player.is_none() {
            l.player = Some(Player::new());
        }
        let key = (secure::short_id(&peer.id) as u64) << 32 | (source as u64) << 24 | (sender as u64 & 0xFF_FFFF);
        if l.audio_logged & (1 << source.min(7)) == 0 { l.audio_logged |= 1 << source.min(7); crate::logi!("first audio packet source={source}"); }
        l.player.as_ref().unwrap().push(key, 0.0, gain, opus, seq);
    }

    pub async fn lead_read_screen(self: Arc<Self>, peer: Arc<Peer>, mut s: quinn::RecvStream, kind: u8) -> Result<()> {
        let mut monitor = [0u8; 1];
        s.read_exact(&mut monitor).await?;
        let thumb = kind == proto::UNI_THUMB;
        loop {
            let mut t = [0u8; 1];
            s.read_exact(&mut t).await?;
            match t[0] {
                proto::FRAME_SIZE => {
                    let mut b = [0u8; 4];
                    s.read_exact(&mut b).await?;
                    if !thumb {
                        let (w, h) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
                        let mut l = self.lead.lock();
                        let v = l.targets.entry(peer.id.clone()).or_default();
                        v.screen = ScreenBuf { width: w as u32, height: h as u32, epoch: v.screen.epoch + 1, tiles: HashMap::new() };
                    }
                }
                proto::FRAME_TILE => {
                    let mut b = [0u8; 8];
                    s.read_exact(&mut b).await?;
                    let len = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                    if len > 32 << 20 {
                        anyhow::bail!("tile too large");
                    }
                    let (x, y) = (u16::from_le_bytes([b[4], b[5]]) as u32, u16::from_le_bytes([b[6], b[7]]) as u32);
                    let mut jpeg = vec![0u8; len];
                    s.read_exact(&mut jpeg).await?;
                    let Some((w, h, rgba)) = decode_jpeg(&jpeg) else { continue };
                    let mut l = self.lead.lock();
                    let v = l.targets.entry(peer.id.clone()).or_default();
                    if thumb {
                        v.thumb = Some((w, h, rgba));
                        v.thumb_rev += 1;
                    } else {
                        v.screen.tiles.insert((x, y), Tile { w, h, rgba });
                    }
                    drop(l);
                    request_repaint(&self);
                }
                _ => anyhow::bail!("bad frame"),
            }
        }
    }

    pub fn lead_on_secure_message(self: &Arc<Self>, peer: &Arc<Peer>, m: Msg) {
        let mut l = self.lead.lock();
        let t = l.targets.entry(peer.id.clone()).or_default();
        match m {
            Msg::ScreenInfo(mons) => {
                if t.monitors.is_empty() {
                    t.monitors = mons;
                }
            }
            Msg::ControlState { holder } => t.secure_holder = holder,
            Msg::Cursor { monitor, x, y } if t.locked || self.peer(&peer.id).is_none() => t.cursor = Some((monitor, x, y)),
            _ => return,
        }
        drop(l);
        request_repaint(self);
    }

    pub fn lead_secure_disconnected(self: &Arc<Self>, id: &str) {
        let mut l = self.lead.lock();
        if let Some(t) = l.targets.get_mut(id) {
            t.secure_mode = Mode::default();
            t.secure_holder.clear();
            t.secure_control_sent = false;
        }
    }

    /// Show secure screens through the system-level process whenever the normal program cannot.
    fn lead_secure_route(self: &Arc<Self>) {
        if !self.has_full_access() {
            return;
        }
        let me = self.identity.id.clone();
        let links: Vec<Arc<Peer>> = self.secure_peers.lock().values().cloned().collect();
        for p in links {
            let normal = self.peer(&p.id).is_some();
            let (send_mode, send_control) = {
                let mut l = self.lead.lock();
                let t = l.targets.entry(p.id.clone()).or_default();
                let secure_screen = (t.locked || !normal) && t.mode.screen != ScreenLevel::Off;
                let mode = if secure_screen { Mode { listen: false, ..t.mode } } else { Mode::default() };
                let send_mode = (mode != t.secure_mode).then_some(mode);
                t.secure_mode = mode;
                // Control passes through this process too, so it also reaches administrator windows.
                let control = t.want_control && (t.control_holder == me || t.locked || !normal);
                let send_control = (control != t.secure_control_sent).then_some(control);
                t.secure_control_sent = control;
                (send_mode, send_control)
            };
            if let Some(mode) = send_mode {
                p.send(Msg::SetMode(mode));
                if mode.screen != ScreenLevel::Off {
                    let name = self.member_name(&p.id);
                    self.audit_log(&name, "Захищений екран (повний доступ)");
                }
            }
            if let Some(want) = send_control {
                p.send(Msg::ControlRequest(want));
            }
        }
    }

    pub fn lead_send_sas(self: &Arc<Self>, target: &str) {
        if let Some(p) = self.secure_peer(target) {
            p.send(Msg::SendSas);
            let name = self.member_name(target);
            self.audit_log(&name, "Натиснуто Ctrl+Alt+Del");
        }
    }

    pub fn lead_set_mode(self: &Arc<Self>, target: &str, mode: Mode) {
        let previous = {
            let mut l = self.lead.lock();
            let t = l.targets.entry(target.to_string()).or_default();
            let prev = t.mode;
            t.mode = mode;
            if mode.screen != ScreenLevel::Full {
                t.want_control = false;
            }
            if mode.screen == ScreenLevel::Off {
                t.screen = ScreenBuf { epoch: t.screen.epoch + 1, ..Default::default() };
            }
            prev
        };
        if mode.screen == ScreenLevel::Off && !mode.listen {
            self.lead.lock().targets.entry(target.to_string()).or_default().consent = 0;
        }
        if let Some(p) = self.peer(target) {
            p.send(Msg::SetMode(mode));
        }
        if previous != mode {
            let describe = |m: Mode| {
                let screen = match m.screen {
                    ScreenLevel::Off => "без екрана",
                    ScreenLevel::Eco => "низька якість",
                    ScreenLevel::Full => ["максимальна якість", "висока якість", "середня якість", "низька якість"][m.quality.min(3) as usize],
                };
                format!("{}, {}", if m.listen { "слухає" } else { "не слухає" }, screen)
            };
            let name = self.member_name(target);
            self.audit_log(&name, &format!("Режим: {}", describe(mode)));
        }
    }

    pub fn lead_set_thumbs(self: &Arc<Self>, on: bool) {
        {
            let mut l = self.lead.lock();
            if l.thumbs_on == on {
                return;
            }
            l.thumbs_on = on;
        }
        for p in self.peers.lock().values() {
            p.send(Msg::SetThumb(on));
        }
    }

    pub fn lead_draw(self: &Arc<Self>, target: &str, op: DrawOp) {
        let me = self.identity.id.clone();
        {
            let mut l = self.lead.lock();
            let t = l.targets.entry(target.to_string()).or_default();
            apply_draw(t, &me, color_for(&me), op.clone());
        }
        if let Some(p) = self.peer(target) {
            p.send(Msg::Draw { from: me, op });
        }
    }

    pub fn lead_send_pointer(&self, target: &str, monitor: u8, x: f32, y: f32, on: bool, size: f32) {
        if let Some(p) = self.peer(target) {
            p.send(Msg::Pointer { monitor, x, y, on, size });
        }
    }

    /// Shows short rings at the place the lead clicked.
    pub fn lead_send_tap(&self, target: &str, x: f32, y: f32) {
        let monitor = self.lead.lock().targets.get(target).map(|t| t.mode.monitor).unwrap_or(0);
        if let Some(p) = self.peer(target) {
            p.send(Msg::Tap { monitor, x, y });
        }
    }

    /// Going to the tray with the close button ends every session.
    pub fn lead_passive(self: &Arc<Self>) {
        let ids: Vec<String> = self.lead.lock().targets.iter().filter(|(_, t)| t.mode.listen || t.mode.screen != ScreenLevel::Off).map(|(k, _)| k.clone()).collect();
        for id in ids {
            self.lead_set_mode(&id, Mode::default());
        }
        self.lead_set_voice_target(None);
    }

    pub fn lead_next_draw_id(&self, target: &str) -> u32 {
        let mut l = self.lead.lock();
        let t = l.targets.entry(target.to_string()).or_default();
        t.next_draw += 1;
        t.next_draw
    }

    pub fn lead_send_hint(self: &Arc<Self>, target: &str, text: String) {
        let id = {
            let mut l = self.lead.lock();
            l.next_hint += 1;
            let id = l.next_hint;
            l.targets.entry(target.to_string()).or_default().hints.push(HintEntry { id, text: text.clone(), ts: now_ms(), state: 0 });
            id
        };
        if let Some(p) = self.peer(target) {
            p.send(Msg::Hint { id, text: text.clone() });
        }
        let name = self.member_name(target);
        self.audit_log(&name, &format!("Підказка: {text}"));
    }

    pub fn lead_take_help(self: &Arc<Self>, target: &str, id: u32) {
        let name = self.my_name();
        {
            let mut l = self.lead.lock();
            if let Some(h) = l.help.iter_mut().find(|h| h.target == target && h.id == id) {
                h.taken_by = name.clone();
            }
        }
        if let Some(p) = self.peer(target) {
            p.send(Msg::HelpTaken { id, by: self.identity.id.clone(), name });
        }
        let who = self.member_name(target);
        self.audit_log(&who, "Взято запит на допомогу");
    }

    pub fn lead_dismiss_help(&self, target: &str, id: u32) {
        self.lead.lock().help.retain(|h| !(h.target == target && h.id == id));
    }

    pub fn lead_request_control(self: &Arc<Self>, target: &str, want: bool) {
        self.lead.lock().targets.entry(target.to_string()).or_default().want_control = want;
        if let Some(p) = self.peer(target) {
            p.send(Msg::ControlRequest(want));
        }
        let who = self.member_name(target);
        self.audit_log(&who, if want { "Запит на керування" } else { "Завершено керування" });
    }

    pub fn lead_input(&self, target: &str, ev: InputEv) {
        let secure = self.lead.lock().targets.get(target).map(|t| t.secure_holder == self.identity.id).unwrap_or(false);
        let peer = if secure { self.secure_peer(target) } else { self.peer(target) };
        if let Some(p) = peer {
            p.send(Msg::Input(ev));
        }
    }

    /// Choose whose session the microphone is for. The mic itself stays closed until the lead opens it.
    pub fn lead_set_voice_target(self: &Arc<Self>, target: Option<String>) {
        MIC_LATCHED.store(false, Ordering::Relaxed);
        MIC_BUTTON.store(false, Ordering::Relaxed);
        let conn = target.as_deref().and_then(|t| self.peer(t)).map(|p| p.conn.clone());
        let me = secure::short_id(&self.identity.id);
        let mut l = self.lead.lock();
        l.active = target;
        match conn {
            None => {
                if let Some(v) = l.voice.take() {
                    v.stop.store(true, Ordering::Relaxed);
                }
            }
            Some(c) => {
                if let Some(v) = &l.voice {
                    *v.target.lock() = Some(c);
                } else {
                    let run = VoiceRun { stop: Arc::new(AtomicBool::new(false)), target: Arc::new(Mutex::new(Some(c))) };
                    let t = run.target.clone();
                    let seq = AtomicU16::new(0);
                    audio::run_capture(CaptureKind::Microphone, run.stop.clone(), move |opus| {
                        if !MIC_OPEN.load(Ordering::Relaxed) {
                            return;
                        }
                        if !VOICE_SENT_LOGGED.swap(true, Ordering::Relaxed) {
                            crate::logi!("voice: first packet sent to the employee");
                        }
                        let n = seq.fetch_add(1, Ordering::Relaxed);
                        let packet = proto::audio_packet(proto::SRC_LEAD_VOICE, me, n, opus);
                        if let Some(c) = t.lock().as_ref() {
                            audio::send_voice(c, bytes::Bytes::from(packet));
                        }
                    });
                    l.voice = Some(run);
                }
            }
        }
    }

    pub fn lead_create_join_code(&self, role: Role, group: String) -> String {
        let code = secure::generate_join_code();
        self.join_codes.lock().push(JoinCode {
            code: code.clone(),
            role,
            group,
            expires_ms: now_ms() + 30 * 60 * 1000,
        });
        code
    }

    pub fn lead_lock_console(self: &Arc<Self>) {
        let (targets, player_drop) = {
            let mut l = self.lead.lock();
            l.console_locked = true;
            let t: Vec<String> = l.targets.keys().cloned().collect();
            (t, l.player.take())
        };
        drop(player_drop);
        for id in targets {
            self.lead_set_mode(&id, Mode::default());
        }
        self.lead_set_thumbs(false);
        self.lead_set_voice_target(None);
    }

    pub fn lead_unlock_console(&self) {
        self.lead.lock().console_locked = false;
    }

    fn poll_lead_clipboard(self: &Arc<Self>) {
        let seq = sysutil::clipboard_sequence();
        let target = {
            let mut l = self.lead.lock();
            let changed = seq != l.clip_seq;
            l.clip_seq = seq;
            if !changed {
                return;
            }
            let Some(active) = l.active.clone() else { return };
            // Copied text goes to the employee only while this lead controls the PC.
            if l.targets.get(&active).map(|t| t.control_holder != self.identity.id).unwrap_or(true) {
                return;
            }
            active
        };
        let Some(text) = sysutil::clipboard_get() else { return };
        if self.lead.lock().clip_applied.take().as_deref() == Some(&text) {
            return;
        }
        if text.len() < 1 << 20 {
            if let Some(p) = self.peer(&target) {
                p.send(Msg::Clipboard(text));
            }
        }
    }

    pub async fn lead_tick_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let mut n = 0u64;
        let mut toggle_was_down = false;
        loop {
            tick.tick().await;
            n += 1;
            if !self.is_lead() || self.acting_as_agent() {
                continue;
            }
            let (ptt, toggle) = {
                let st = self.state.read();
                (st.lead.ptt_key, st.lead.mic_toggle_key)
            };
            let down = sysutil::key_is_down(toggle);
            if down && !toggle_was_down {
                MIC_LATCHED.fetch_xor(true, Ordering::Relaxed);
            }
            toggle_was_down = down;
            let locked = self.lead.lock().console_locked;
            let open = !locked && (sysutil::key_is_down(ptt) || MIC_LATCHED.load(Ordering::Relaxed) || MIC_BUTTON.load(Ordering::Relaxed));
            if MIC_OPEN.swap(open, Ordering::Relaxed) != open {
                request_repaint(&self);
            }
            if n % 25 == 0 {
                self.poll_lead_clipboard();
            }
            if n % 10 == 0 {
                self.lead_secure_route();
                let now = now_ms();
                let mut l = self.lead.lock();
                let mut animate = false;
                let fade = self.state.read().directory.policy.stroke_fade_sec as i64 * 1000;
                for t in l.targets.values_mut() {
                    t.strokes.retain(|s| !stroke_expired(s, now, fade));
                    animate |= !t.strokes.is_empty();
                }
                drop(l);
                if animate {
                    request_repaint(&self);
                }
            }
        }
    }
}

pub const FADE_MS: i64 = 1500;

pub fn stroke_expired(s: &Stroke, now: i64, fade_after_ms: i64) -> bool {
    s.ended_ms.map(|e| now - e > fade_after_ms + FADE_MS).unwrap_or(false)
}

pub fn apply_draw(t: &mut TargetView, lead: &str, color: [u8; 3], op: DrawOp) {
    let now = now_ms();
    match op {
        DrawOp::Begin { id, tool, monitor, x, y, width, alpha } => t.strokes.push(Stroke {
            lead: lead.to_string(),
            id,
            tool,
            monitor,
            points: vec![[x, y]],
            ended_ms: None,
            color,
            stamps: vec![now],
            width,
            alpha,
        }),
        DrawOp::Point { id, x, y } => {
            if let Some(s) = t.strokes.iter_mut().rev().find(|s| s.lead == lead && s.id == id) {
                if matches!(s.tool, Tool::Rect | Tool::Arrow) && s.points.len() == 2 {
                    s.points[1] = [x, y];
                } else {
                    s.points.push([x, y]);
                    s.stamps.push(now);
                }
            }
        }
        DrawOp::End { id } => {
            if let Some(s) = t.strokes.iter_mut().rev().find(|s| s.lead == lead && s.id == id) {
                s.ended_ms = Some(now);
            }
        }
        DrawOp::Cancel { id } => t.strokes.retain(|s| !(s.lead == lead && s.id == id)),
        DrawOp::ClearMine => t.strokes.retain(|s| s.lead != lead),
        DrawOp::ClearAll => t.strokes.clear(),
    }
}
