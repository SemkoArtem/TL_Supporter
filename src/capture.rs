//! Screen capture. Each screen is cut into tiles and only changed tiles are compressed.
//! Each viewer tracks which tile versions it already has, so a late joiner gets a full picture
//! and a slow viewer simply skips intermediate versions. Nothing runs when nobody is subscribed.

use crate::proto::{MonitorInfo, ScreenLevel};
use anyhow::{anyhow, Context, Result};
use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Profile {
    Max,
    High,
    Medium,
    Low,
    Thumb,
}

impl Profile {
    pub fn from_level(level: ScreenLevel, quality: u8) -> Option<Self> {
        match (level, quality) {
            (ScreenLevel::Off, _) => None,
            (ScreenLevel::Eco, _) => Some(Profile::Low),
            (ScreenLevel::Full, 1) => Some(Profile::High),
            (ScreenLevel::Full, 2) => Some(Profile::Medium),
            (ScreenLevel::Full, 3) => Some(Profile::Low),
            (ScreenLevel::Full, _) => Some(Profile::Max),
        }
    }

    fn interval(self) -> Duration {
        match self {
            Profile::Max => Duration::from_millis(33),
            Profile::High => Duration::from_millis(40),
            Profile::Medium => Duration::from_millis(66),
            Profile::Low => Duration::from_millis(125),
            // Previews of the whole team do not need to be fresh.
            Profile::Thumb => Duration::from_millis(5000),
        }
    }

    fn quality(self) -> u8 {
        match self {
            Profile::Max => 92,
            Profile::High => 80,
            Profile::Medium => 65,
            Profile::Low => 45,
            Profile::Thumb => 55,
        }
    }

    fn scale_div(self, width: u32) -> u32 {
        match self {
            Profile::Max | Profile::High | Profile::Medium => 1,
            Profile::Low => 2,
            Profile::Thumb => (width / 320).max(1),
        }
    }

    fn tile(self) -> u32 {
        match self {
            Profile::Thumb => u32::MAX,
            _ => 128,
        }
    }
}

const PROFILES: [Profile; 5] = [Profile::Max, Profile::High, Profile::Medium, Profile::Low, Profile::Thumb];

#[derive(Default)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub tile: u32,
    pub versions: Vec<u64>,
    pub jpegs: Vec<Option<Arc<Vec<u8>>>>,
}

pub struct ProfilePub {
    subs: AtomicUsize,
    pub snap: RwLock<Snapshot>,
    pub tx: watch::Sender<u64>,
}

struct Monitor {
    pubs: HashMap<Profile, Arc<ProfilePub>>,
    running: Mutex<bool>,
}

pub struct CaptureHub {
    monitors: Mutex<HashMap<u8, Arc<Monitor>>>,
    /// Follow the input desktop, used by the system-level process for secure screens.
    secure: bool,
}

/// Holding this keeps capture running. Dropping it releases the subscription.
pub struct Subscription {
    pub publisher: Arc<ProfilePub>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.publisher.subs.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CaptureHub {
    pub fn new(secure: bool) -> Self {
        Self { monitors: Mutex::new(HashMap::new()), secure }
    }

    pub fn subscribe(&self, monitor: u8, profile: Profile) -> Subscription {
        let m = {
            let mut map = self.monitors.lock();
            map.entry(monitor)
                .or_insert_with(|| {
                    let pubs = PROFILES
                        .iter()
                        .map(|p| {
                            let (tx, _) = watch::channel(0u64);
                            (*p, Arc::new(ProfilePub { subs: AtomicUsize::new(0), snap: RwLock::new(Snapshot::default()), tx }))
                        })
                        .collect();
                    Arc::new(Monitor { pubs, running: Mutex::new(false) })
                })
                .clone()
        };
        let publisher = m.pubs[&profile].clone();
        publisher.subs.fetch_add(1, Ordering::SeqCst);
        let mut running = m.running.lock();
        if !*running {
            *running = true;
            let worker = m.clone();
            let secure = self.secure;
            std::thread::spawn(move || {
                worker_loop(monitor, &worker, secure);
                *worker.running.lock() = false;
                // A subscriber may have arrived between the loop ending and the flag clearing.
                if worker.pubs.values().any(|p| p.subs.load(Ordering::SeqCst) > 0) {
                    let again = worker.clone();
                    *worker.running.lock() = true;
                    std::thread::spawn(move || {
                        worker_loop(monitor, &again, secure);
                        *again.running.lock() = false;
                    });
                }
            });
        }
        Subscription { publisher }
    }
}

struct Duplication {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

fn find_output(index: u8) -> Result<(IDXGIAdapter1, IDXGIOutput)> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut n = 0u8;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                let desc = output.GetDesc()?;
                if desc.AttachedToDesktop.as_bool() {
                    if n == index {
                        return Ok((adapter, output));
                    }
                    n += 1;
                }
                o += 1;
            }
            a += 1;
        }
        Err(anyhow!("monitor {index} not found"))
    }
}

type MonitorRects = Vec<(i32, i32, i32, i32)>;

static RECTS: Mutex<Option<(Instant, MonitorRects)>> = parking_lot::const_mutex(None);

/// Desktop rectangle of each monitor: left, top, width, height. Asked for on every mouse
/// movement of a lead, so the list is kept for a moment instead of being read each time.
pub fn monitor_rects() -> MonitorRects {
    let mut cached = RECTS.lock();
    if let Some((at, rects)) = cached.as_ref() {
        if at.elapsed() < Duration::from_secs(2) {
            return rects.clone();
        }
    }
    let rects = read_monitor_rects();
    *cached = Some((Instant::now(), rects.clone()));
    rects
}

fn read_monitor_rects() -> MonitorRects {
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(desc) = output.GetDesc() {
                    if desc.AttachedToDesktop.as_bool() {
                        let r = desc.DesktopCoordinates;
                        out.push((r.left, r.top, r.right - r.left, r.bottom - r.top));
                    }
                }
                o += 1;
            }
            a += 1;
        }
    }
    out
}

pub fn list_monitors() -> Vec<MonitorInfo> {
    monitor_rects()
        .iter()
        .enumerate()
        .map(|(i, r)| MonitorInfo {
            index: i as u8,
            width: r.2 as u32,
            height: r.3 as u32,
            primary: r.0 == 0 && r.1 == 0,
        })
        .collect()
}

fn open_duplication(index: u8) -> Result<Duplication> {
    unsafe {
        let (adapter, output) = find_output(index)?;
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device: ID3D11Device = device.context("no d3d device")?;
        let context = context.context("no d3d context")?;
        let output1: IDXGIOutput1 = output.cast()?;
        let dup = output1.DuplicateOutput(&device)?;
        let desc = dup.GetDesc();
        let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        let tex = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        device.CreateTexture2D(&tex, None, Some(&mut staging))?;
        Ok(Duplication { device, context, dup, staging: staging.context("no staging texture")?, width, height })
    }
}

/// Returns true when `frame` was replaced with a new image.
fn grab(d: &Duplication, frame: &mut Vec<u8>) -> Result<bool> {
    unsafe {
        let _ = &d.device;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        match d.dup.AcquireNextFrame(150, &mut info, &mut resource) {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        let changed = info.LastPresentTime != 0 || frame.is_empty();
        if changed {
            let resource = resource.context("no frame")?;
            let tex: ID3D11Texture2D = resource.cast()?;
            d.context.CopyResource(&d.staging, &tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            d.context.Map(&d.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let row = d.width as usize * 4;
            frame.resize(row * d.height as usize, 0);
            for y in 0..d.height as usize {
                let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
                std::ptr::copy_nonoverlapping(src, frame.as_mut_ptr().add(y * row), row);
            }
            d.context.Unmap(&d.staging, 0);
        }
        d.dup.ReleaseFrame()?;
        Ok(changed)
    }
}

/// Slower capture for when desktop duplication is refused on a secure screen.
fn grab_gdi(rect: (i32, i32, i32, i32), frame: &mut Vec<u8>) -> bool {
    use windows::Win32::Graphics::Gdi::*;
    let (w, h) = (rect.2, rect.3);
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(screen);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let mut ok = false;
        if let Ok(bitmap) = CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(memory, bitmap);
            if BitBlt(memory, 0, 0, w, h, screen, rect.0, rect.1, SRCCOPY).is_ok() {
                let size = (w * h * 4) as usize;
                frame.resize(size, 0);
                std::ptr::copy_nonoverlapping(bits as *const u8, frame.as_mut_ptr(), size);
                ok = true;
            }
            SelectObject(memory, old);
            let _ = DeleteObject(bitmap);
        }
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        ok
    }
}

/// Threads for scaling and compressing the screen: half of the cores, so that the employee's
/// own work keeps the rest of the PC. They keep the normal priority: below it Windows can hold
/// them back for seconds while other programs are busy, and the lead would see no picture.
fn encode_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        rayon::ThreadPoolBuilder::new()
            .num_threads((cores / 2).max(1))
            .thread_name(|i| format!("tls-encode-{i}"))
            .build()
            .expect("encode thread pool")
    })
}

/// Averaging downscale of BGRA by an integer factor.
pub fn downscale(src: &[u8], w: u32, h: u32, div: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = ((w / div).max(1), (h / div).max(1));
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    let row = (w * 4) as usize;
    let div_us = div as usize;
    encode_pool().install(|| out.par_chunks_mut((nw * 4) as usize).enumerate().for_each(|(y, line)| {
        for x in 0..nw as usize {
            let mut sum = [0u32; 3];
            for sy in 0..div_us {
                let base = (y * div_us + sy) * row + x * div_us * 4;
                for sx in 0..div_us {
                    let p = base + sx * 4;
                    sum[0] += src[p] as u32;
                    sum[1] += src[p + 1] as u32;
                    sum[2] += src[p + 2] as u32;
                }
            }
            let n = (div * div) as u32;
            line[x * 4] = (sum[0] / n) as u8;
            line[x * 4 + 1] = (sum[1] / n) as u8;
            line[x * 4 + 2] = (sum[2] / n) as u8;
            line[x * 4 + 3] = 255;
        }
    }));
    (out, nw, nh)
}

fn encode_tile(pixels: &[u8], w: u16, h: u16, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(4096);
    let mut enc = jpeg_encoder::Encoder::new(&mut out, quality);
    if quality >= 70 {
        enc.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
    }
    enc.encode(pixels, w, h, jpeg_encoder::ColorType::Bgra).ok()?;
    Some(out)
}

pub struct ProfileState {
    prev: Vec<u8>,
    last: Option<Instant>,
}

/// Find changed tiles in `image`, compress them and return `(index, jpeg)` pairs.
pub fn diff_and_encode(prev: &mut Vec<u8>, image: &[u8], w: u32, h: u32, tile: u32, quality: u8) -> Vec<(usize, Vec<u8>)> {
    let tile_w = tile.min(w);
    let tile_h = tile.min(h);
    let cols = w.div_ceil(tile_w);
    let rows = h.div_ceil(tile_h);
    let first = prev.len() != image.len();
    if first {
        prev.clear();
        prev.resize(image.len(), 0);
    }
    let row_bytes = (w * 4) as usize;
    let mut changed = Vec::new();
    for ry in 0..rows {
        for cx in 0..cols {
            let (x, y) = (cx * tile_w, ry * tile_h);
            let (cw, ch) = (tile_w.min(w - x), tile_h.min(h - y));
            let mut differs = first;
            if !differs {
                for line in y..y + ch {
                    let start = line as usize * row_bytes + x as usize * 4;
                    let end = start + cw as usize * 4;
                    if image[start..end] != prev[start..end] {
                        differs = true;
                        break;
                    }
                }
            }
            if differs {
                changed.push(((ry * cols + cx) as usize, x, y, cw, ch));
            }
        }
    }
    for &(_, x, y, cw, ch) in &changed {
        for line in y..y + ch {
            let start = line as usize * row_bytes + x as usize * 4;
            let end = start + cw as usize * 4;
            prev[start..end].copy_from_slice(&image[start..end]);
        }
    }
    encode_pool().install(|| {
        changed
            .par_iter()
            .filter_map(|&(idx, x, y, cw, ch)| {
                let mut buf = Vec::with_capacity((cw * ch * 4) as usize);
                for line in y..y + ch {
                    let start = line as usize * row_bytes + x as usize * 4;
                    buf.extend_from_slice(&image[start..start + cw as usize * 4]);
                }
                encode_tile(&buf, cw as u16, ch as u16, quality).map(|j| (idx, j))
            })
            .collect()
    })
}

fn process(profile: Profile, state: &mut ProfileState, publisher: &ProfilePub, frame: &[u8], w: u32, h: u32) {
    let div = profile.scale_div(w);
    let scaled;
    let (image, iw, ih): (&[u8], u32, u32) = if div > 1 {
        scaled = downscale(frame, w, h, div);
        (&scaled.0, scaled.1, scaled.2)
    } else {
        (frame, w, h)
    };
    let tile = profile.tile().min(iw.max(ih));
    let results = diff_and_encode(&mut state.prev, image, iw, ih, tile, profile.quality());
    if results.is_empty() {
        return;
    }
    let cols = iw.div_ceil(tile.min(iw));
    let rows = ih.div_ceil(tile.min(ih));
    let count = (cols * rows) as usize;
    let mut snap = publisher.snap.write();
    if snap.width != iw || snap.height != ih || snap.versions.len() != count {
        snap.width = iw;
        snap.height = ih;
        snap.tile = tile;
        snap.versions = vec![0; count];
        snap.jpegs = vec![None; count];
    }
    for (idx, jpeg) in results {
        snap.versions[idx] += 1;
        snap.jpegs[idx] = Some(Arc::new(jpeg));
    }
    drop(snap);
    publisher.tx.send_modify(|v| *v += 1);
}

fn worker_loop(index: u8, monitor: &Monitor, secure: bool) {
    let mut gdi_rect: Option<(i32, i32, i32, i32)> = None;
    let mut next_duplication_try = Instant::now();
    let mut states: HashMap<Profile, ProfileState> =
        PROFILES.iter().map(|p| (*p, ProfileState { prev: Vec::new(), last: None })).collect();
    let mut frame = Vec::new();
    let mut dup: Option<Duplication> = None;
    let mut idle_since: Option<Instant> = None;
    loop {
        let any = monitor.pubs.values().any(|p| p.subs.load(Ordering::SeqCst) > 0);
        if !any {
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(800) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        idle_since = None;
        // Copying a whole screen is the expensive part, so it waits until a picture is due.
        let now = Instant::now();
        let mut next_due: Option<Instant> = None;
        for p in PROFILES {
            if monitor.pubs[&p].subs.load(Ordering::SeqCst) > 0 {
                let due = states[&p].last.map(|l| l + p.interval()).unwrap_or(now);
                next_due = Some(next_due.map_or(due, |d| d.min(due)));
            } else {
                let s = states.get_mut(&p).unwrap();
                s.prev.clear();
                s.last = None;
            }
        }
        if let Some(due) = next_due.filter(|d| *d > now) {
            std::thread::sleep((due - now).min(Duration::from_millis(100)));
            continue;
        }
        if dup.is_none() && Instant::now() >= next_duplication_try {
            if secure {
                crate::sysutil::attach_input_desktop();
            }
            match open_duplication(index) {
                Ok(d) => {
                    frame.clear();
                    gdi_rect = None;
                    dup = Some(d);
                }
                Err(e) if secure => {
                    crate::logi!("duplication refused on secure screen, using GDI: {e}");
                    gdi_rect = monitor_rects().get(index as usize).copied();
                    next_duplication_try = Instant::now() + Duration::from_secs(3);
                }
                Err(e) => {
                    crate::logi!("capture open failed on monitor {index}: {e}");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        let mut changed = true;
        let (w, h) = if let Some(d) = dup.as_ref() {
            let size = (d.width, d.height);
            match grab(d, &mut frame) {
                Ok(c) => changed = c,
                Err(e) => {
                    crate::logi!("capture lost: {e}");
                    dup = None;
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
            }
            size
        } else if let Some(rect) = gdi_rect {
            crate::sysutil::attach_input_desktop();
            if !grab_gdi(rect, &mut frame) {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            std::thread::sleep(Duration::from_millis(200));
            (rect.2 as u32, rect.3 as u32)
        } else {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };
        if frame.is_empty() {
            continue;
        }
        for p in PROFILES {
            let publisher = &monitor.pubs[&p];
            if publisher.subs.load(Ordering::SeqCst) == 0 {
                // A new subscriber to this profile must start from a complete picture.
                let s = states.get_mut(&p).unwrap();
                s.prev.clear();
                s.last = None;
                continue;
            }
            let s = states.get_mut(&p).unwrap();
            if s.last.map(|l| l.elapsed() < p.interval()).unwrap_or(false) {
                continue;
            }
            s.last = Some(Instant::now());
            if !changed && !s.prev.is_empty() {
                continue;
            }
            process(p, s, publisher, &frame, w, h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, value: u8) -> Vec<u8> {
        vec![value; (w * h * 4) as usize]
    }

    #[test]
    fn first_frame_sends_all_tiles() {
        let mut prev = Vec::new();
        let r = diff_and_encode(&mut prev, &image(300, 200, 100), 300, 200, 128, 70);
        assert_eq!(r.len(), 3 * 2);
    }

    #[test]
    fn unchanged_frame_sends_nothing() {
        let mut prev = Vec::new();
        let img = image(300, 200, 100);
        diff_and_encode(&mut prev, &img, 300, 200, 128, 70);
        assert!(diff_and_encode(&mut prev, &img, 300, 200, 128, 70).is_empty());
    }

    #[test]
    fn only_touched_tile_is_sent() {
        let mut prev = Vec::new();
        let mut img = image(300, 200, 100);
        diff_and_encode(&mut prev, &img, 300, 200, 128, 70);
        let p = (150 * 300 + 200) * 4;
        img[p] = 7;
        let r = diff_and_encode(&mut prev, &img, 300, 200, 128, 70);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, 3 + 1);
    }

    #[test]
    fn jpeg_tile_decodes() {
        let mut prev = Vec::new();
        let r = diff_and_encode(&mut prev, &image(128, 128, 200), 128, 128, 128, 80);
        let mut dec = zune_jpeg::JpegDecoder::new(&r[0].1);
        let px = dec.decode().unwrap();
        assert!(!px.is_empty());
    }

    #[test]
    fn downscale_averages() {
        let mut src = image(4, 2, 0);
        src[0] = 100;
        src[4] = 100;
        let (out, w, h) = downscale(&src, 4, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out[0], 50);
    }
}
