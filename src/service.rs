//! Windows service and installer. The service keeps the program running in the user's session,
//! restarts it if it is closed and applies verified updates.

use crate::util::{data_dir, wide};
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::{DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId, TOKEN_ALL_ACCESS};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_service::service::*;
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "TLSupporter";
/// Exit code that means "stopped on purpose, do not restart until the next sign-in".
pub const EXIT_STOPPED: u32 = 77;
/// Exit code that means "another copy already runs in this session".
pub const EXIT_BUSY: u32 = 78;
/// A program that ended this quickly did not really start, so the service waits before retrying.
const QUICK_EXIT: Duration = Duration::from_secs(5);

/// The more often a start fails, the longer the service waits, up to five minutes.
fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((2u64 << failures.min(8)).min(300))
}

/// Close every other running copy of this program. A copy left over from an earlier launch
/// would keep its old windows on screen and make the service try again and again.
fn kill_other_copies(keep: &[u32]) {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return };
        let me = GetCurrentProcessId();
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            let pid = entry.th32ProcessID;
            if name.eq_ignore_ascii_case("TLSupporter.exe") && pid != me && !keep.contains(&pid) {
                if let Ok(process) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    if TerminateProcess(process, 1).is_ok() {
                        crate::logi!("closed a leftover copy of the program, pid {pid}");
                    }
                    let _ = CloseHandle(process);
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
}

define_windows_service!(ffi_service_main, service_main);

pub fn run_dispatcher() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = service_body() {
        crate::logi!("service failed: {e}");
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}

struct Child {
    process: HANDLE,
    pid: u32,
    session: u32,
    started: Instant,
}

/// How a watched process ended.
struct Ended {
    code: u32,
    session: u32,
    quick: bool,
}

fn start_in_session(session: u32, exe: &str) -> Result<Child> {
    unsafe {
        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token).context("no user signed in")?;
        let mut env = std::ptr::null_mut();
        let _ = CreateEnvironmentBlock(&mut env, token, false);
        let mut desktop = wide("winsta0\\default");
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        let app = wide(exe);
        let mut command = wide(&format!("\"{exe}\" --from-service"));
        let result = CreateProcessAsUserW(
            token,
            PCWSTR(app.as_ptr()),
            PWSTR(command.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT,
            Some(env),
            None,
            &startup,
            &mut info,
        );
        if !env.is_null() {
            let _ = DestroyEnvironmentBlock(env);
        }
        let _ = CloseHandle(token);
        result?;
        let _ = CloseHandle(info.hThread);
        Ok(Child { process: info.hProcess, pid: info.dwProcessId, session, started: Instant::now() })
    }
}

/// Start the secure screen process as SYSTEM inside the console session. It runs even when
/// nobody is signed in, which is what makes the sign-in screen reachable.
fn start_system_agent(session: u32, exe: &str) -> Result<Child> {
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own)?;
        let mut token = HANDLE::default();
        let duplicated = DuplicateTokenEx(own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut token);
        let _ = CloseHandle(own);
        duplicated?;
        SetTokenInformation(token, TokenSessionId, &session as *const u32 as *const _, 4)?;
        let mut desktop = wide("winsta0\\default");
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        let app = wide(exe);
        let mut command = wide(&format!("\"{exe}\" --system-agent"));
        let result = CreateProcessAsUserW(
            token,
            PCWSTR(app.as_ptr()),
            PWSTR(command.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &startup,
            &mut info,
        );
        let _ = CloseHandle(token);
        result?;
        let _ = CloseHandle(info.hThread);
        Ok(Child { process: info.hProcess, pid: info.dwProcessId, session, started: Instant::now() })
    }
}

fn stop_child(child: &mut Option<Child>) {
    if let Some(c) = child.take() {
        unsafe {
            let _ = TerminateProcess(c.process, 0);
            let _ = CloseHandle(c.process);
        }
    }
}

/// Clears `child` if it has ended or belongs to another session. Says how it ended, if it did.
fn check_child(child: &mut Option<Child>, session: u32) -> Option<Ended> {
    let c = child.as_ref()?;
    let mut code = 0u32;
    unsafe {
        let _ = GetExitCodeProcess(c.process, &mut code);
    }
    if code != STILL_ACTIVE.0 as u32 {
        let ended = Ended { code, session: c.session, quick: c.started.elapsed() < QUICK_EXIT };
        unsafe {
            let _ = CloseHandle(c.process);
        }
        *child = None;
        return Some(ended);
    }
    if c.session != session {
        stop_child(child);
    }
    None
}

fn apply_update(exe: &str) -> bool {
    let Some(verified) = crate::update::pending_update() else { return false };
    // Written next to the program, where ordinary users cannot change it.
    let (new, old) = (format!("{exe}.new"), format!("{exe}.old"));
    if std::fs::write(&new, verified).is_err() {
        let _ = std::fs::remove_file(&new);
        return false;
    }
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(exe, &old).is_err() {
        let _ = std::fs::remove_file(&new);
        return false;
    }
    if std::fs::rename(&new, exe).is_err() {
        let _ = std::fs::rename(&old, exe);
        return false;
    }
    crate::logi!("update installed");
    true
}

fn service_body() -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = service_control_handler::register(SERVICE_NAME, move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            flag.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0))?;

    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let _ = std::fs::remove_file(format!("{exe}.old"));
    kill_other_copies(&[]);
    let mut child: Option<Child> = None;
    let mut system_child: Option<Child> = None;
    let mut stopped_in: Option<u32> = None;
    let mut child_retry = Instant::now();
    let mut system_retry = Instant::now();
    let (mut child_failures, mut system_failures) = (0u32, 0u32);
    let mut last_update_check = Instant::now();
    let mut updated = false;

    while !stop.load(Ordering::SeqCst) && !updated {
        std::thread::sleep(Duration::from_secs(1));
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if session == u32::MAX {
            continue;
        }
        if stopped_in.is_some() && stopped_in != Some(session) {
            stopped_in = None;
        }
        if let Some(ended) = check_child(&mut child, session) {
            if ended.code == EXIT_STOPPED {
                stopped_in = Some(ended.session);
                // Nothing of the program may stay open, such as its settings window.
                let keep: Vec<u32> = system_child.iter().map(|c| c.pid).collect();
                kill_other_copies(&keep);
            } else if ended.code == EXIT_BUSY {
                // Something outside the service holds the program open, so it goes first.
                let keep: Vec<u32> = system_child.iter().map(|c| c.pid).collect();
                kill_other_copies(&keep);
                child_failures += 1;
                child_retry = Instant::now() + retry_delay(child_failures);
            } else if ended.quick {
                child_failures += 1;
                child_retry = Instant::now() + retry_delay(child_failures);
            } else {
                child_failures = 0;
            }
        }
        if let Some(ended) = check_child(&mut system_child, session) {
            if ended.quick {
                system_failures += 1;
                system_retry = Instant::now() + retry_delay(system_failures);
            } else {
                system_failures = 0;
            }
        }
        if stopped_in.is_some() {
            // Closing the program on purpose also closes secure access.
            stop_child(&mut system_child);
        } else {
            if system_child.is_none() && Instant::now() >= system_retry {
                match start_system_agent(session, &exe) {
                    Ok(c) => system_child = Some(c),
                    Err(e) => crate::logi!("secure agent start failed: {e}"),
                }
            }
            // Fails while nobody is signed in, which is fine: the next pass tries again.
            if child.is_none() && Instant::now() >= child_retry {
                if let Ok(c) = start_in_session(session, &exe) {
                    child = Some(c);
                }
            }
        }
        if last_update_check.elapsed() > Duration::from_secs(30) {
            last_update_check = Instant::now();
            if crate::update::pending_update().is_some() {
                stop_child(&mut child);
                stop_child(&mut system_child);
                std::thread::sleep(Duration::from_secs(1));
                updated = apply_update(&exe);
            }
        }
    }
    stop_child(&mut child);
    stop_child(&mut system_child);
    // A non-zero exit after an update makes Windows restart the service with the new file.
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty(), if updated { 1 } else { 0 }))?;
    if updated {
        std::process::exit(1);
    }
    Ok(())
}

fn is_running() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .ok()
        .and_then(|m| m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS).ok())
        .and_then(|s| s.query_status().ok())
        .is_some_and(|s| s.current_state == ServiceState::Running)
}

/// Close this copy and open it again. The service does that by itself when it is running.
pub fn restart_program() -> ! {
    if !is_running() {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--restarted").spawn();
        }
    }
    std::process::exit(0)
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin().as_bool() }
}

/// Start this same program with administrator rights and the given arguments.
pub fn relaunch_elevated(args: &str) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let (verb, file, params) = (wide("runas"), wide(&exe.to_string_lossy()), wide(args));
    unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR(params.as_ptr()), None, SW_SHOWNORMAL).0 as isize > 32 }
}

fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into())).join("TLSupporter")
}

fn run(cmd: &str, args: &[&str]) {
    let _ = std::process::Command::new(cmd).args(args).output();
}

/// Copy the program, open the firewall for the local subnet only and register the service.
pub fn install() -> Result<()> {
    if !is_admin() {
        bail!("потрібні права адміністратора");
    }
    kill_other_copies(&[]);
    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join("TLSupporter.exe");
    let current = std::env::current_exe()?;
    if current != target {
        std::fs::copy(&current, &target)?;
    }
    let data = data_dir();
    run("icacls", &[&data.to_string_lossy(), "/grant", "*S-1-5-32-545:(OI)(CI)M", "/T", "/Q"]);
    let exe = target.to_string_lossy().to_string();
    // Lets a service press Ctrl+Alt+Del on behalf of a lead with full access.
    run("reg", &["add", r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System", "/v", "SoftwareSASGeneration", "/t", "REG_DWORD", "/d", "1", "/f"]);
    run("netsh", &["advfirewall", "firewall", "delete", "rule", "name=TLSupporter"]);
    run(
        "netsh",
        &["advfirewall", "firewall", "add", "rule", "name=TLSupporter", "dir=in", "action=allow", &format!("program={exe}"), "enable=yes", "profile=any", "remoteip=localsubnet"],
    );
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("TL Supporter"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target,
        launch_arguments: vec![OsString::from("--service")],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let service = manager.create_service(&info, ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
    run("sc", &["failure", SERVICE_NAME, "reset=60", "actions=restart/2000/restart/2000/restart/5000"]);
    service.start::<OsString>(&[])?;
    Ok(())
}

pub fn uninstall() -> Result<()> {
    if !is_admin() {
        bail!("потрібні права адміністратора");
    }
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    if let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE) {
        let _ = service.stop();
        std::thread::sleep(Duration::from_secs(2));
        service.delete()?;
    }
    kill_other_copies(&[]);
    run("netsh", &["advfirewall", "firewall", "delete", "rule", "name=TLSupporter"]);
    let _ = std::fs::remove_dir_all(install_dir());
    Ok(())
}

pub fn is_installed() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .ok()
        .and_then(|m| m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS).ok())
        .is_some()
}
