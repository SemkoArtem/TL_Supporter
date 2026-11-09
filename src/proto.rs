//! Wire protocol. Messages are named-field MessagePack, so fields can be added later
//! without breaking older versions. A message type an older peer does not know is skipped.

use crate::model::{Directory, LogEntry, Role};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
/// Oldest peer protocol this build can work with.
pub const PROTOCOL_MIN: u32 = 1;

pub const DEFAULT_DISCOVERY_PORT: u16 = 47700;
pub const DEFAULT_QUIC_PORT: u16 = 47701;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ScreenLevel {
    #[default]
    Off,
    Eco,
    Full,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Mode {
    pub listen: bool,
    pub screen: ScreenLevel,
    pub monitor: u8,
    /// 0 maximum, 1 high, 2 medium, 3 low.
    pub quality: u8,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tool {
    Marker,
    MarkerSemi,
    Rect,
    Arrow,
    Laser,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DrawOp {
    Begin {
        id: u32,
        tool: Tool,
        monitor: u8,
        x: f32,
        y: f32,
        /// Line width in screen pixels, 0 means the default of the tool.
        #[serde(default)]
        width: f32,
        /// Opacity from 0 to 1, 0 means the default of the tool.
        #[serde(default)]
        alpha: f32,
    },
    Point { id: u32, x: f32, y: f32 },
    End { id: u32 },
    Cancel { id: u32 },
    ClearMine,
    ClearAll,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum InputEv {
    Move { x: f32, y: f32 },
    Button { button: u8, down: bool },
    Wheel { dy: f32 },
    Key { vk: u16, down: bool },
    Text { ch: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct MonitorInfo {
    pub index: u8,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct LeadActivity {
    pub id: String,
    pub name: String,
    pub watching: bool,
    pub listening: bool,
    pub drawing: bool,
    pub speaking: bool,
    pub controlling: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Hello {
    pub protocol: u32,
    pub app_version: String,
    pub id: String,
    pub name: String,
    pub role: Role2,
    pub lead_proof_b64: String,
    pub team_id: String,
    /// Set by the system-level process that serves secure screens.
    pub secure_channel: bool,
}

/// Separate from `model::Role` so `Default` is explicit on the wire.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Role2 {
    #[default]
    Agent,
    Lead,
}

impl From<Role> for Role2 {
    fn from(r: Role) -> Self {
        match r {
            Role::Agent => Role2::Agent,
            Role::Lead => Role2::Lead,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Msg {
    Hello(Hello),
    DirSync(Box<Directory>),
    LogSync(Vec<LogEntry>),
    SetMode(Mode),
    SetThumb(bool),
    ScreenInfo(Vec<MonitorInfo>),
    Draw { from: String, op: DrawOp },
    Hint { id: u32, text: String },
    HintClosed { id: u32 },
    /// 1 read, 2 accepted, 3 rejected, 4 ignored.
    HintAck { id: u32, state: u8 },
    /// A pointer that only shows where the lead is looking, it leaves no drawing.
    Pointer { monitor: u8, x: f32, y: f32, on: bool, size: f32 },
    /// A click of the lead, shown as short rings on the employee's screen.
    Tap { monitor: u8, x: f32, y: f32 },
    /// Answer of a lead's PC to the first connection: 0 allowed, 1 waiting, 2 refused.
    ConsentState { state: u8 },
    HelpRequest { id: u32, reason: String },
    HelpTaken { id: u32, by: String, name: String },
    HelpCancel { id: u32 },
    ControlRequest(bool),
    ControlState { holder: String },
    Input(InputEv),
    Clipboard(String),
    Cursor { monitor: u8, x: f32, y: f32 },
    Activity(Vec<LeadActivity>),
    Status { locked: bool },
    Rename { name: String },
    SendSas,
}

const MAX_MSG: usize = 16 * 1024 * 1024;

pub fn encode(msg: &Msg) -> Result<Vec<u8>> {
    let body = rmp_serde::to_vec_named(msg)?;
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Reads one frame. `Ok(None)` means the frame arrived but is a type this version does not know.
pub async fn read_msg(rx: &mut quinn::RecvStream) -> Result<Option<Msg>> {
    let mut len = [0u8; 4];
    rx.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MSG {
        return Err(anyhow!("message too large"));
    }
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body).await?;
    Ok(rmp_serde::from_slice(&body).ok())
}

/// Audio datagram: `[source][sender u32][seq u16][opus...]`.
pub const SRC_AGENT_MIC: u8 = 0;
pub const SRC_AGENT_SYSTEM: u8 = 1;
pub const SRC_LEAD_VOICE: u8 = 2;
const AUDIO_HEADER: usize = 7;

pub fn audio_packet(source: u8, sender: u32, seq: u16, opus: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(AUDIO_HEADER + opus.len());
    p.push(source);
    p.extend_from_slice(&sender.to_le_bytes());
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(opus);
    p
}

pub fn parse_audio(p: &[u8]) -> Option<(u8, u32, u16, &[u8])> {
    if p.len() <= AUDIO_HEADER {
        return None;
    }
    let sender = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
    let seq = u16::from_le_bytes([p[5], p[6]]);
    Some((p[0], sender, seq, &p[AUDIO_HEADER..]))
}

/// First byte of every unidirectional stream.
pub const UNI_SCREEN: u8 = 1;
pub const UNI_THUMB: u8 = 2;
pub const UNI_UPDATE: u8 = 3;

/// Screen stream header followed by tile frames.
pub const FRAME_SIZE: u8 = 0;
pub const FRAME_TILE: u8 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_packet_roundtrip() {
        let p = audio_packet(SRC_LEAD_VOICE, 7, 513, &[1, 2, 3]);
        assert_eq!(parse_audio(&p), Some((SRC_LEAD_VOICE, 7, 513, &[1u8, 2, 3][..])));
        assert_eq!(parse_audio(&p[..7]), None);
    }

    #[test]
    fn message_with_extra_fields_still_decodes() {
        #[derive(Serialize)]
        struct FutureMode {
            listen: bool,
            screen: ScreenLevel,
            monitor: u8,
            new_field: u32,
        }
        let bytes = rmp_serde::to_vec_named(&FutureMode {
            listen: true,
            screen: ScreenLevel::Full,
            monitor: 1,
            new_field: 9,
        })
        .unwrap();
        let m: Mode = rmp_serde::from_slice(&bytes).unwrap();
        assert!(m.listen);
        assert_eq!(m.screen, ScreenLevel::Full);
        assert_eq!(m.quality, 0, "a missing field means maximum quality");
    }

    #[test]
    fn unknown_message_type_is_not_an_error() {
        #[derive(Serialize)]
        enum Future {
            BrandNew { x: u32 },
        }
        let bytes = rmp_serde::to_vec_named(&Future::BrandNew { x: 1 }).unwrap();
        assert!(rmp_serde::from_slice::<Msg>(&bytes).is_err());
    }
}
