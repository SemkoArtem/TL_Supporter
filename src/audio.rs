//! WASAPI capture and playback with Opus compression.
//! The computer-sound capture excludes this program, so hints never echo back to the lead.

use anyhow::{anyhow, Context, Result};
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Channels, SampleRate};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use windows::core::{implement, Interface, IUnknown, HRESULT, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;

pub const FRAME_SAMPLES: usize = 960; // 20 ms at 48 kHz
const SAMPLE_RATE: u32 = 48_000;
const BUFFER_100NS: i64 = 400_000; // 40 ms

const STREAMFLAGS_AUTOCONVERT: u32 = 0x8000_0000 | 0x0800_0000; // AUTOCONVERTPCM | SRC_DEFAULT_QUALITY

struct ComGuard;

impl ComGuard {
    fn new() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        ComGuard
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn pcm_format(channels: u16) -> WAVEFORMATEX {
    let block = channels * 2;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: channels,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block as u32,
        nBlockAlign: block,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

/// Devices chosen in the settings: microphone and speakers. Empty means the Windows default.
static DEVICES: Mutex<(String, String)> = parking_lot::const_mutex((String::new(), String::new()));
static MIC_LEVEL: AtomicU32 = AtomicU32::new(0);
static SEND_LOGGED: AtomicBool = AtomicBool::new(false);
static MIC_FAILED: AtomicBool = AtomicBool::new(false);
static MIC_LAST_SOUND: AtomicI64 = AtomicI64::new(0);
/// Volume of the lead's voice on the employee's PC, as the bits of an f32.
static VOICE_GAIN: AtomicU32 = AtomicU32::new(0x3F80_0000);

const VOICE_CFG: &str = "voice.cfg";

const CONNECT_CFG: &str = "connect_sound.cfg";
/// The employee can make the lead quieter, but must always be able to hear them.
pub const MIN_VOICE_VOLUME: f32 = 0.25;
static MIC_GAIN: AtomicU32 = AtomicU32::new(0x3F80_0000);

fn read_volume(file: &str, min: f32) -> f32 {
    std::fs::read_to_string(crate::util::data_dir().join(file))
        .ok()
        .and_then(|t| t.split_whitespace().next().and_then(|v| v.parse::<f32>().ok()))
        .unwrap_or(1.0)
        .clamp(min, 1.0)
}

pub fn load_voice_volume() -> f32 {
    read_volume(VOICE_CFG, MIN_VOICE_VOLUME)
}

pub fn save_voice_volume(volume: f32) {
    let _ = std::fs::write(crate::util::data_dir().join(VOICE_CFG), format!("{:.2}", volume.clamp(MIN_VOICE_VOLUME, 1.0)));
    apply_voice_cfg();
}

/// Volume of the sound played when a lead connects. Zero turns it off.
pub fn connect_volume() -> f32 {
    read_volume(CONNECT_CFG, 0.0)
}

pub fn save_connect_volume(volume: f32) {
    let _ = std::fs::write(crate::util::data_dir().join(CONNECT_CFG), format!("{:.2}", volume.clamp(0.0, 1.0)));
}

/// Picks up a change made in the separate settings window.
pub fn apply_voice_cfg() {
    VOICE_GAIN.store(load_voice_volume().to_bits(), Ordering::Relaxed);
}

/// How much this lead's own microphone is amplified.
pub fn set_mic_gain(gain: f32) {
    MIC_GAIN.store(gain.clamp(0.1, 3.0).to_bits(), Ordering::Relaxed);
}

pub fn voice_gain() -> f32 {
    f32::from_bits(VOICE_GAIN.load(Ordering::Relaxed))
}

/// The microphone could not be opened at all.
pub fn mic_failed() -> bool {
    MIC_FAILED.load(Ordering::Relaxed)
}

/// When the microphone last picked up anything, in milliseconds of `now_ms`.
pub fn mic_last_sound() -> i64 {
    MIC_LAST_SOUND.load(Ordering::Relaxed)
}

pub fn set_devices(mic: &str, speaker: &str) {
    *DEVICES.lock() = (mic.to_string(), speaker.to_string());
}

/// Loudest recent sample of the microphone, from 0 to 1.
pub fn mic_level() -> f32 {
    MIC_LEVEL.load(Ordering::Relaxed) as f32 / 32768.0
}

/// Datagrams report failure only here, so the first problem goes to the log.
pub fn send_voice(c: &quinn::Connection, data: bytes::Bytes) {
    if let Err(e) = c.send_datagram(data) {
        if !SEND_LOGGED.swap(true, Ordering::Relaxed) {
            crate::logi!("audio send failed: {e}");
        }
    }
}

fn device_name(dev: &IMMDevice) -> String {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    unsafe {
        dev.OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "?".to_string())
    }
}

/// The device people actually hear and speak into. The separate "communications" role
/// often points at another device, which sounded like no audio at all.
fn default_device(flow: EDataFlow) -> Result<IMMDevice> {
    unsafe {
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let chosen = {
            let d = DEVICES.lock();
            if flow == eCapture { d.0.clone() } else { d.1.clone() }
        };
        if !chosen.is_empty() {
            let id: Vec<u16> = chosen.encode_utf16().chain(std::iter::once(0)).collect();
            match e.GetDevice(PCWSTR(id.as_ptr())) {
                Ok(d) => return Ok(d),
                Err(err) => crate::logi!("chosen audio device is gone ({err}), using the default"),
            }
        }
        Ok(e.GetDefaultAudioEndpoint(flow, eConsole)?)
    }
}

fn enumerate(flow: EDataFlow) -> Result<Vec<(String, String)>> {
    unsafe {
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let list = e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        let mut out = Vec::new();
        for i in 0..list.GetCount()? {
            let dev = list.Item(i)?;
            let raw = dev.GetId()?;
            let id = raw.to_string().unwrap_or_default();
            CoTaskMemFree(Some(raw.0 as *const _));
            out.push((id, device_name(&dev)));
        }
        Ok(out)
    }
}

/// Microphones and speakers as (id, name). Runs on its own thread because of COM.
pub fn list_devices() -> (Vec<(String, String)>, Vec<(String, String)>) {
    std::thread::spawn(|| {
        let _com = ComGuard::new();
        (enumerate(eCapture).unwrap_or_default(), enumerate(eRender).unwrap_or_default())
    })
    .join()
    .unwrap_or_default()
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivateHandler {
    done: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        unsafe {
            let _ = SetEvent(self.done);
        }
        Ok(())
    }
}

#[repr(C)]
struct RawBlobVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    cb: u32,
    data: *const u8,
    pad: usize,
}

/// Loopback of every process except this one. Needs Windows 10 2004 or newer.
fn process_loopback_client(format: &WAVEFORMATEX) -> Result<IAudioClient> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: GetCurrentProcessId(),
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        let blob = RawBlobVariant {
            vt: 65, // VT_BLOB
            r1: 0,
            r2: 0,
            r3: 0,
            cb: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            data: &params as *const _ as *const u8,
            pad: 0,
        };
        let done = CreateEventW(None, false, false, None)?;
        let handler: IActivateAudioInterfaceCompletionHandler = ActivateHandler { done }.into();
        let path: Vec<u16> = "VAD\\Process_Loopback\0".encode_utf16().collect();
        let op = ActivateAudioInterfaceAsync(
            PCWSTR(path.as_ptr()),
            &IAudioClient::IID,
            Some(&blob as *const _ as *const _),
            &handler,
        )?;
        WaitForSingleObject(done, 3000);
        let _ = CloseHandle(done);
        let mut result = HRESULT(0);
        let mut unknown: Option<IUnknown> = None;
        op.GetActivateResult(&mut result, &mut unknown)?;
        result.ok()?;
        let client: IAudioClient = unknown.context("no audio client")?.cast()?;
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | STREAMFLAGS_AUTOCONVERT,
            BUFFER_100NS,
            0,
            format,
            None,
        )?;
        Ok(client)
    }
}

fn device_client(flow: EDataFlow, extra: u32, format: &WAVEFORMATEX) -> Result<IAudioClient> {
    unsafe {
        let dev = default_device(flow)?;
        crate::logi!("audio device {}: {}", if flow == eCapture { "input" } else { "output" }, device_name(&dev));
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            extra | STREAMFLAGS_AUTOCONVERT,
            BUFFER_100NS,
            0,
            format,
            None,
        )?;
        Ok(client)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureKind {
    Microphone,
    ComputerSound,
}

/// Runs until `stop` is set, calling `on_packet` with Opus packets of 20 ms.
pub fn run_capture(
    kind: CaptureKind,
    stop: Arc<AtomicBool>,
    mut on_packet: impl FnMut(&[u8]) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _com = ComGuard::new();
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = capture_once(kind, &stop, &mut on_packet) {
                crate::logi!("audio capture {kind:?} failed: {e}");
                if kind == CaptureKind::Microphone {
                    MIC_FAILED.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(std::time::Duration::from_millis(1500));
            }
        }
    })
}

fn capture_once(kind: CaptureKind, stop: &AtomicBool, on_packet: &mut impl FnMut(&[u8])) -> Result<()> {
    let channels: u16 = if kind == CaptureKind::ComputerSound { 2 } else { 1 };
    let format = pcm_format(channels);
    let (client, event_ok) = match kind {
        CaptureKind::Microphone => (device_client(eCapture, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, &format)?, true),
        CaptureKind::ComputerSound => match process_loopback_client(&format) {
            Ok(c) => { crate::logi!("computer sound capture excludes this program"); (c, true) }
            Err(e) => {
                crate::logi!("process loopback unavailable ({e}), using plain loopback");
                (device_client(eRender, AUDCLNT_STREAMFLAGS_LOOPBACK, &format)?, false)
            }
        },
    };
    if kind == CaptureKind::Microphone {
        MIC_FAILED.store(false, Ordering::Relaxed);
    }
    unsafe {
        let event = CreateEventW(None, false, false, None)?;
        if event_ok {
            client.SetEventHandle(event)?;
        }
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
            .map_err(|e| anyhow!("opus encoder: {e}"))?;
        let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 4);
        let mut output = vec![0u8; 1500];
        let mut heard = false;
        let result = (|| -> Result<()> {
            while !stop.load(Ordering::Relaxed) {
                if event_ok {
                    WaitForSingleObject(event, 40);
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                loop {
                    let size = capture.GetNextPacketSize()?;
                    if size == 0 {
                        break;
                    }
                    let mut data = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    if silent {
                        pending.extend(std::iter::repeat(0).take(frames as usize));
                    } else {
                        let s = std::slice::from_raw_parts(data as *const i16, frames as usize * channels as usize);
                        if channels == 2 {
                            pending.extend(s.chunks_exact(2).map(|c| ((c[0] as i32 + c[1] as i32) / 2) as i16));
                        } else {
                            pending.extend_from_slice(s);
                        }
                    }
                    capture.ReleaseBuffer(frames)?;
                }
                while pending.len() >= FRAME_SAMPLES {
                    let mut frame: Vec<i16> = pending.drain(..FRAME_SAMPLES).collect();
                    if kind == CaptureKind::Microphone {
                        let gain = f32::from_bits(MIC_GAIN.load(Ordering::Relaxed));
                        if gain != 1.0 {
                            for s in frame.iter_mut() {
                                *s = (*s as f32 * gain).clamp(-32768.0, 32767.0) as i16;
                            }
                        }
                    }
                    let peak = frame.iter().map(|s| s.unsigned_abs() as u32).max().unwrap_or(0);
                    if kind == CaptureKind::Microphone {
                        MIC_LEVEL.store(peak, Ordering::Relaxed);
                        if peak > 30 {
                            MIC_LAST_SOUND.store(crate::util::now_ms(), Ordering::Relaxed);
                        }
                    }
                    if peak > 30 && !heard {
                        heard = true;
                        crate::logi!("audio capture {kind:?}: sound detected");
                    }
                    // Silence is not sent at all.
                    if frame.iter().any(|s| s.unsigned_abs() > 30) {
                        let n = encoder.encode(&frame, &mut output).map_err(|e| anyhow!("opus: {e}"))?;
                        on_packet(&output[..n]);
                    }
                }
            }
            Ok(())
        })();
        let _ = client.Stop();
        let _ = CloseHandle(event);
        result
    }
}

struct Source {
    decoder: Decoder,
    queue: VecDeque<i16>,
    pan_left: f32,
    pan_right: f32,
    gain: f32,
    started: bool,
    last_seq: Option<u16>,
}

const START_SAMPLES: usize = 1920; // 40 ms of jitter buffer before playback starts
const MAX_SAMPLES: usize = 9600; // beyond 200 ms the queue is trimmed

fn apply_packet(src: &mut Source, packet: &[u8], seq: u16) {
    if let Some(last) = src.last_seq {
        let delta = seq.wrapping_sub(last);
        if delta == 0 || delta > 32768 {
            return; // duplicate or late
        }
        // Fill short gaps with Opus concealment.
        for _ in 1..delta.min(4) {
            let mut pcm = vec![0i16; FRAME_SAMPLES];
            let lost: Option<audiopus::packet::Packet<'_>> = None;
            if let Ok(signals) = (&mut pcm[..]).try_into() {
                if let Ok(n) = src.decoder.decode(lost, signals, false) {
                    src.queue.extend(&pcm[..n]);
                }
            }
        }
    }
    src.last_seq = Some(seq);
    let mut pcm = vec![0i16; FRAME_SAMPLES * 3];
    let (Ok(p), Ok(signals)) = (
        <&[u8] as TryInto<audiopus::packet::Packet<'_>>>::try_into(packet),
        <&mut [i16] as TryInto<audiopus::MutSignals<'_, i16>>>::try_into(&mut pcm[..]),
    ) else {
        return;
    };
    if let Ok(n) = src.decoder.decode(Some(p), signals, false) {
        src.queue.extend(&pcm[..n]);
    }
    if src.queue.len() > MAX_SAMPLES {
        let excess = src.queue.len() - START_SAMPLES;
        src.queue.drain(..excess);
    }
}

fn mix_into(sources: &mut HashMap<u64, Source>, out: &mut [i16]) {
    let frames = out.len() / 2;
    let mut mixed = vec![0f32; frames * 2];
    for src in sources.values_mut() {
        if !src.started {
            if src.queue.len() < START_SAMPLES {
                continue;
            }
            src.started = true;
        }
        for i in 0..frames {
            let Some(s) = src.queue.pop_front() else {
                src.started = false;
                break;
            };
            mixed[i * 2] += s as f32 * src.pan_left * src.gain;
            mixed[i * 2 + 1] += s as f32 * src.pan_right * src.gain;
        }
    }
    for (o, m) in out.iter_mut().zip(mixed) {
        *o = m.clamp(-32768.0, 32767.0) as i16;
    }
}

/// Plays any number of voice streams mixed together. Stops its thread when dropped.
pub struct Player {
    sources: Arc<Mutex<HashMap<u64, Source>>>,
    stop: Arc<AtomicBool>,
}

impl Player {
    pub fn new() -> Self {
        let sources = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, t) = (sources.clone(), stop.clone());
        std::thread::spawn(move || {
            let _com = ComGuard::new();
            while !t.load(Ordering::Relaxed) {
                if let Err(e) = render_once(&s, &t) {
                    crate::logi!("audio playback failed: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                }
            }
        });
        Self { sources, stop }
    }

    /// `pan` is -1 for left ear only, 0 for both, 1 for right ear only.
    pub fn push(&self, key: u64, pan: f32, gain: f32, packet: &[u8], seq: u16) {
        let mut map = self.sources.lock();
        if !map.contains_key(&key) {
            let Ok(decoder) = Decoder::new(SampleRate::Hz48000, Channels::Mono) else { return };
            map.insert(
                key,
                Source { decoder, queue: VecDeque::new(), pan_left: 1.0, pan_right: 1.0, gain: 1.0, started: false, last_seq: None },
            );
        }
        let src = map.get_mut(&key).unwrap();
        src.pan_left = if pan > 0.0 { 0.0 } else { 1.0 };
        src.pan_right = if pan < 0.0 { 0.0 } else { 1.0 };
        src.gain = gain;
        apply_packet(src, packet, seq);
    }

    pub fn remove(&self, key: u64) {
        self.sources.lock().remove(&key);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn render_once(sources: &Mutex<HashMap<u64, Source>>, stop: &AtomicBool) -> Result<()> {
    let format = pcm_format(2);
    let client = device_client(eRender, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, &format)?;
    unsafe {
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let size = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService()?;
        client.Start()?;
        let result = (|| -> Result<()> {
            while !stop.load(Ordering::Relaxed) {
                WaitForSingleObject(event, 50);
                let padding = client.GetCurrentPadding()?;
                let free = size - padding;
                if free == 0 {
                    continue;
                }
                let data = render.GetBuffer(free)?;
                let out = std::slice::from_raw_parts_mut(data as *mut i16, free as usize * 2);
                mix_into(&mut sources.lock(), out);
                render.ReleaseBuffer(free, 0)?;
            }
            Ok(())
        })();
        let _ = client.Stop();
        let _ = CloseHandle(event);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            decoder: Decoder::new(SampleRate::Hz48000, Channels::Mono).unwrap(),
            queue: VecDeque::new(),
            pan_left: 1.0,
            pan_right: 1.0,
            gain: 1.0,
            started: false,
            last_seq: None,
        }
    }

    #[test]
    fn two_sources_play_together_with_their_own_volume() {
        let mut map = HashMap::new();
        let mut mic = source();
        mic.queue.extend(std::iter::repeat(1000).take(START_SAMPLES));
        let mut system = source();
        system.gain = 0.5;
        system.queue.extend(std::iter::repeat(400).take(START_SAMPLES));
        map.insert(1u64, mic);
        map.insert(2u64, system);
        let mut out = vec![0i16; 8];
        mix_into(&mut map, &mut out);
        assert_eq!(out[0], 1200, "both sources are heard at once");
        assert_eq!(out[1], 1200);
    }

    #[test]
    fn opus_roundtrip_gives_one_frame() {
        let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip).unwrap();
        let tone: Vec<i16> = (0..FRAME_SAMPLES).map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16).collect();
        let mut out = vec![0u8; 1500];
        let n = enc.encode(&tone, &mut out).unwrap();
        let mut s = source();
        apply_packet(&mut s, &out[..n], 1);
        assert_eq!(s.queue.len(), FRAME_SAMPLES);
        apply_packet(&mut s, &out[..n], 1);
        assert_eq!(s.queue.len(), FRAME_SAMPLES, "duplicate packet is ignored");
    }

    #[test]
    fn mixer_waits_for_jitter_buffer_and_pans() {
        let mut map = HashMap::new();
        let mut s = source();
        s.pan_right = 0.0;
        s.queue.extend(std::iter::repeat(1000).take(START_SAMPLES - 1));
        map.insert(1u64, s);
        let mut out = vec![0i16; 8];
        mix_into(&mut map, &mut out);
        assert_eq!(out[0], 0, "not enough buffered yet");
        map.get_mut(&1).unwrap().queue.push_back(1000);
        mix_into(&mut map, &mut out);
        assert_eq!(out[0], 1000);
        assert_eq!(out[1], 0, "right channel is muted");
    }
}
