//! Test commands for running several copies on one PC. They work only when `TLS_DATA_DIR`
//! is set, so a normal installation cannot use them.

use crate::agent_ui::UiHandle;
use crate::model::{self, Directory, LocalState, MemberState, Rev, Role};
use crate::proto::{DrawOp, Mode, ScreenLevel, Tool};
use crate::secure::{self, Identity};
use anyhow::{bail, Result};
use std::time::{Duration, Instant};

pub fn create_team(identity: &Identity, team: &str, password: &str) -> Result<()> {
    let directory = Directory::new(team, identity, "Керівник Тест", secure::hash_password(password))?;
    model::save_state(&LocalState { display_name: "Керівник Тест".into(), directory, ..Default::default() })
}

pub fn employee_mode(value: &str) -> Result<()> {
    let mut st = model::load_state();
    st.work_as_agent = value == "on";
    model::save_state(&st)
}

/// Act as a lead: change the role of the first other PC once it is online.
pub fn set_role(identity: Identity, action: &str) -> Result<()> {
    let state = model::load_state();
    let my_id = identity.id.clone();
    let (ui, rx) = UiHandle::new();
    std::thread::spawn(move || while rx.recv().is_ok() {});
    let node = crate::node::start(identity, state, ui)?;
    let target = |online: bool| {
        node.state.read().directory.members.values().find(|m| m.id != my_id && (!online || node.peer(&m.id).is_some())).map(|m| m.id.clone())
    };
    let id = if action.ends_with("-off") {
        target(false)
    } else {
        wait_for(40, || target(true).is_some());
        target(true)
    };
    let Some(id) = id else { bail!("no other PC found") };
    match action {
        "lead" => node.set_member_role(&id, Role::Lead),
        "agent" | "agent-off" => node.set_member_role(&id, Role::Agent),
        "revoke" | "revoke-off" => node.revoke_member(&id),
        _ => {}
    }
    std::thread::sleep(Duration::from_secs(5));
    let st = node.state.read();
    let m = st.directory.member(&id).cloned().unwrap_or_default();
    println!("ROLE {:?} state={:?} admin={} group_set={}", m.role, m.state, m.rights.admin, !m.group.is_empty());
    std::process::exit(0);
}

/// A session out of sight stops its picture and gets it back, with control, when shown again.
pub fn park(identity: Identity) -> Result<()> {
    let state = model::load_state();
    let me = identity.id.clone();
    let (ui, rx) = UiHandle::new();
    std::thread::spawn(move || while rx.recv().is_ok() {});
    let node = crate::node::start(identity, state, ui)?;
    let online = || node.state.read().directory.members.values().find(|m| m.id != me && node.peer(&m.id).is_some()).map(|m| m.id.clone());
    wait_for(40, || online().is_some());
    let Some(target) = online() else { bail!("no other PC came online") };
    let watching = || node.lead.lock().targets.get(&target).is_some_and(|t| t.activity.iter().any(|a| a.id == me && a.watching));
    let holder = || node.lead.lock().targets.get(&target).map(|t| t.control_holder.clone()).unwrap_or_default();
    let epoch = || node.lead.lock().targets.get(&target).map(|t| t.screen.epoch).unwrap_or(0);
    let tiles = || node.lead.lock().targets.get(&target).map(|t| t.screen.tiles.len()).unwrap_or(0);
    node.lead_set_mode(&target, Mode { listen: false, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false });
    println!("WATCHING {:?}", wait_for(10, || watching()));
    println!("FIRST_PICTURE {:?}", wait_for(10, || tiles() > 0));
    node.lead_request_control(&target, true);
    println!("CONTROL {:?}", wait_for(5, || holder() == me));
    node.lead_set_parked(&target, true);
    println!("PARKED_NOT_WATCHING {:?}", wait_for(5, || !watching()));
    println!("PARKED_CONTROL_RELEASED {:?}", wait_for(5, || holder().is_empty()));
    let before = epoch();
    node.lead_set_parked(&target, false);
    println!("BACK_WATCHING {:?}", wait_for(10, || watching()));
    println!("BACK_NEW_PICTURE {:?}", wait_for(10, || epoch() > before && tiles() > 0));
    println!("BACK_CONTROL {:?}", wait_for(5, || holder() == me));
    std::process::exit(0);
}

/// The steps of a factory reset, without the window around them.
pub fn factory_reset_request(password: &str) -> Result<()> {
    let released = model::request_team_release(password);
    println!("RELEASED {released}");
    if released {
        model::factory_reset()?;
    }
    Ok(())
}

/// Does what the service does for a factory reset.
pub fn release_worker(secs: u64) -> Result<()> {
    let mut gate = model::ReleaseGate::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        model::process_team_release(&mut gate);
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn wait_for(limit_secs: u64, mut check: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(limit_secs) {
        if check() {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Check the switch to the secure screen process. `lock_file` makes the employee's program
/// report a locked Windows, which is what happens on the lock screen or an administrator prompt.
pub fn run_secure_e2e(identity: Identity, lock_file: &str) -> Result<()> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
    let state = model::load_state();
    let my_id = identity.id.clone();
    let (ui, rx) = UiHandle::new();
    std::thread::spawn(move || while rx.recv().is_ok() {});
    let node = crate::node::start(identity, state, ui)?;

    let find = |n: &std::sync::Arc<crate::node::Node>| {
        n.state.read().directory.members.values().find(|m| m.id != my_id && m.role == Role::Agent).map(|m| (m.id.clone(), m.state))
    };
    wait_for(30, || find(&node).is_some());
    let (target, st) = find(&node).ok_or_else(|| anyhow::anyhow!("no agent"))?;
    if st == MemberState::Pending {
        let id = target.clone();
        node.update_directory(|d, me| {
            let m = d.members.get_mut(&id).unwrap();
            m.state = MemberState::Approved;
            m.group = d.groups.keys().next().cloned().unwrap_or_default();
            m.rev = Rev::new(me);
        });
    }
    println!("NORMAL_LINK {:?}", wait_for(30, || node.peer(&target).is_some()));
    println!("SECURE_LINK {:?}", wait_for(30, || node.secure_peer(&target).is_some()));
    node.lead_set_mode(&target, Mode { listen: false, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false });
    let has_tiles = |n: &std::sync::Arc<crate::node::Node>| n.lead.lock().targets.get(&target).map(|t| !t.screen.tiles.is_empty()).unwrap_or(false);
    println!("NORMAL_TILES {:?}", wait_for(10, || has_tiles(&node)));
    println!("SECURE_IDLE_WHILE_UNLOCKED {}", node.lead.lock().targets[&target].secure_mode == Mode::default());

    std::fs::write(lock_file, b"1")?;
    println!("LOCK_SEEN {:?}", wait_for(10, || node.lead.lock().targets[&target].locked));
    println!("SECURE_ROUTED {:?}", wait_for(5, || node.lead.lock().targets[&target].secure_mode.screen == ScreenLevel::Full));
    node.lead.lock().targets.get_mut(&target).unwrap().screen.tiles.clear();
    println!("SECURE_TILES {:?}", wait_for(10, || has_tiles(&node)));

    node.lead_request_control(&target, true);
    println!("SECURE_HOLDER {:?}", wait_for(5, || node.lead.lock().targets[&target].secure_holder == node.identity.id));
    let mut before = POINT::default();
    unsafe { GetCursorPos(&mut before)? };
    node.lead_input(&target, crate::proto::InputEv::Move { x: 0.5, y: 0.5 });
    let rect = crate::capture::monitor_rects()[0];
    let (cx, cy) = (rect.0 + rect.2 / 2, rect.1 + rect.3 / 2);
    let moved = wait_for(3, || {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p).is_ok() && (p.x - cx).abs() <= 3 && (p.y - cy).abs() <= 3 }
    });
    println!("INPUT_APPLIED {moved:?}");
    unsafe { SetCursorPos(before.x, before.y)? };

    node.lead_request_control(&target, false);
    std::fs::remove_file(lock_file)?;
    println!("UNLOCK_SEEN {:?}", wait_for(10, || !node.lead.lock().targets[&target].locked));
    println!("SECURE_STOPPED {:?}", wait_for(5, || node.lead.lock().targets[&target].secure_mode == Mode::default()));

    // A lead without the right must lose the secure link.
    let me = node.identity.id.clone();
    node.update_directory(|d, by| {
        let m = d.members.get_mut(&me).unwrap();
        m.rights.full_access = false;
        m.rev = Rev::new(by);
    });
    println!("SECURE_LINK_DROPPED_WITHOUT_RIGHT {:?}", wait_for(12, || node.secure_peer(&target).is_none()));
    println!("SECURE_E2E_DONE");
    std::process::exit(0);
}

/// Join a team with a one-time code, the same way the first-run window does.
pub fn join(identity: &Identity, code: &str, name: &str) -> Result<()> {
    let directory = crate::node::join_with_code(identity, code, name)?;
    println!("JOINED team={} members={}", directory.team_name, directory.members.len());
    println!("HAS_AUTHORITY_KEY {}", !directory.authority_private_b64.is_empty());
    model::save_state(&LocalState { display_name: name.into(), directory, ..Default::default() })
}

/// Act as a new PC that shows its own code and waits for a lead.
pub fn code_wait(identity: &Identity, code: &str, name: &str) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        if let Ok(directory) = crate::node::try_join(identity, code, name, 1) {
            let me = directory.member(&identity.id).cloned().unwrap_or_default();
            println!("JOINED team={} state={:?} group_set={} authority_key={}", directory.team_name, me.state, !me.group.is_empty(), !directory.authority_private_b64.is_empty());
            return model::save_state(&LocalState { display_name: name.into(), directory, ..Default::default() });
        }
    }
    bail!("no lead answered")
}

/// Act as a lead typing a code on the main screen: find the PC, then add it to a group.
pub fn code_add(identity: Identity, code: &str) -> Result<()> {
    let state = model::load_state();
    let group = state.directory.groups.keys().next().cloned().unwrap_or_default();
    let (ui, rx) = UiHandle::new();
    std::thread::spawn(move || while rx.recv().is_ok() {});
    let node = crate::node::start(identity, state, ui)?;
    node.lead_check_code("WRONG-CODE");
    std::thread::sleep(Duration::from_secs(4));
    println!("WRONG_CODE_FOUND {}", node.lead_check_view().map(|c| !c.found_id.is_empty()).unwrap_or(false));
    node.lead_check_code(code);
    let found = wait_for(30, || node.lead_check_view().is_some_and(|c| !c.found_id.is_empty()));
    let view = node.lead_check_view().unwrap_or_default();
    println!("FOUND after={found:?} name={}", view.found_name);
    println!("MEMBER_BEFORE_ADD {}", node.state.read().directory.member(&view.found_id).is_some());
    node.lead_add_checked(group);
    let added = wait_for(15, || node.lead_check_view().is_some_and(|c| c.added));
    println!("ADDED after={added:?}");
    std::thread::sleep(Duration::from_secs(3));
    let st = node.state.read();
    let m = st.directory.member(&view.found_id).cloned().unwrap_or_default();
    println!("MEMBER role={:?} state={:?} group_set={}", m.role, m.state, !m.group.is_empty());
    std::process::exit(0);
}

/// Run a lead without a window, publish a one-time code in a file and report who joined.
pub fn serve_code(identity: Identity, seconds: u64) -> Result<()> {
    let state = model::load_state();
    let (ui, rx) = UiHandle::new();
    std::thread::spawn(move || while rx.recv().is_ok() {});
    let node = crate::node::start(identity, state, ui)?;
    let code = node.lead_create_join_code(Role::Agent, String::new());
    std::fs::write(crate::util::data_dir().join("join.code"), &code)?;
    std::thread::sleep(Duration::from_secs(seconds));
    let st = node.state.read();
    for m in st.directory.members.values() {
        println!("MEMBER role={:?} state={:?} id={}", m.role, m.state, m.id);
    }
    println!("CODES_LEFT {}", node.join_codes.lock().len());
    std::process::exit(0);
}

/// Act as a lead without a window: find an employee, approve them and exercise every session feature.
pub fn run_e2e(identity: Identity, seconds: u64) -> Result<()> {
    let state = model::load_state();
    let my_id = identity.id.clone();
    let (ui, rx) = UiHandle::new();
    std::thread::spawn(move || while rx.recv().is_ok() {});
    let node = crate::node::start(identity, state, ui)?;
    let deadline = Instant::now() + Duration::from_secs(40);
    let target = loop {
        let found = {
            let st = node.state.read();
            st.directory.members.values().find(|m| m.id != my_id && m.role == Role::Agent && m.state != MemberState::Revoked).map(|m| (m.id.clone(), m.state))
        };
        if let Some((id, state)) = found {
            if state == MemberState::Pending {
                let target = id.clone();
                println!("PENDING agent appeared: {id}");
                node.update_directory(|d, me| {
                    let m = d.members.get_mut(&target).unwrap();
                    m.state = MemberState::Approved;
                    m.name = "Працівник Тест".into();
                    m.group = d.groups.keys().next().cloned().unwrap_or_default();
                    m.rev = Rev::new(me);
                });
            } else if node.peer(&id).is_some() {
                break id;
            }
        }
        if Instant::now() > deadline {
            bail!("agent did not connect in time");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    println!("CONNECTED to {target}");
    std::thread::sleep(Duration::from_millis(1200));
    node.lead_set_voice_target(Some(target.clone()));
    node.lead_set_thumbs(true);
    let started = Instant::now();
    node.lead_set_mode(&target, Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false });
    let mut first_image = None;
    while started.elapsed() < Duration::from_secs(8) {
        if node.lead.lock().targets.get(&target).map(|t| !t.screen.tiles.is_empty()).unwrap_or(false) {
            first_image = Some(started.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("FIRST_TILE after {first_image:?}");
    // Draw a shape of each kind so the overlay can be checked on screen.
    let tools = [(Tool::Marker, 0.15f32), (Tool::MarkerSemi, 0.3), (Tool::Rect, 0.45), (Tool::Arrow, 0.6), (Tool::Laser, 0.75)];
    for (tool, x) in tools {
        let id = node.lead_next_draw_id(&target);
        node.lead_draw(&target, DrawOp::Begin { id, tool, monitor: 0, x, y: 0.3, width: 0.0, alpha: 0.0 });
        for i in 1..=12 {
            node.lead_draw(&target, DrawOp::Point { id, x: x + i as f32 * 0.008, y: 0.3 + i as f32 * 0.02 });
            std::thread::sleep(Duration::from_millis(25));
        }
        if tool != Tool::Laser {
            node.lead_draw(&target, DrawOp::End { id });
        }
    }
    node.lead_send_hint(&target, "Уточни номер договору і перезвони клієнту".into());
    node.lead_request_control(&target, true);
    crate::lead_core::MIC_LATCHED.store(true, std::sync::atomic::Ordering::Relaxed);
    send_test_voice(&node, &target);
    println!("DRAWN_AND_HINT_SENT");
    // A moving pointer and clicks, so both can be seen on the employee's screen.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let t = started.elapsed().as_secs_f32();
        node.lead_send_pointer(&target, 0, 0.3 + (t * 0.8).sin() * 0.1, 0.5, true, 10.0);
        if (t * 10.0) as u32 % 10 == 0 {
            node.lead_send_tap(&target, 0.55, 0.5);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    {
        let l = node.lead.lock();
        if let Some(t) = l.targets.get(&target) {
            println!("HINT_STATES {:?}", t.hints.iter().map(|h| h.state).collect::<Vec<_>>());
            println!("LAST_AUDIO_AGE_MS {}", crate::util::now_ms() - t.last_audio_ms);
        }
    }
    {
        let l = node.lead.lock();
        let t = l.targets.get(&target);
        if let Some(t) = t {
            println!("SCREEN {}x{} tiles_buffered={}", t.screen.width, t.screen.height, t.screen.tiles.len());
            println!("THUMB {:?} revs={}", t.thumb.as_ref().map(|x| (x.0, x.1)), t.thumb_rev);
            println!("MONITORS {:?}", t.monitors);
            println!("CONTROL holder_is_me={}", t.control_holder == my_id);
            println!("HINTS_OPEN {}", t.hints.len());
            println!("CURSOR {:?}", t.cursor);
            println!("LOCKED {}", t.locked);
            println!("ACTIVITY {:?}", t.activity);
        }
    }
    let audit = node.audit.lock().len();
    println!("AUDIT_ENTRIES {audit}");
    node.lead_request_control(&target, false);
    std::thread::sleep(Duration::from_millis(500));
    println!("E2E_DONE");
    std::process::exit(0);
}

/// A short quiet tone as lead voice, independent of whether this PC has a microphone.
fn send_test_voice(node: &std::sync::Arc<crate::node::Node>, target: &str) {
    use audiopus::{coder::Encoder, Application, Channels, SampleRate};
    let Some(peer) = node.peer(target) else { return };
    let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip).unwrap();
    let me = secure::short_id(&node.identity.id);
    let mut out = vec![0u8; 1500];
    for seq in 0..25u16 {
        let frame: Vec<i16> = (0..960).map(|i| ((i as f32 + seq as f32 * 960.0) * 0.06).sin() as f32 * 1200.0).map(|v| v as i16).collect();
        let n = encoder.encode(&frame, &mut out).unwrap();
        let packet = crate::proto::audio_packet(crate::proto::SRC_LEAD_VOICE, me, seq, &out[..n]);
        let _ = peer.conn.send_datagram(bytes::Bytes::from(packet));
        std::thread::sleep(Duration::from_millis(20));
    }
}