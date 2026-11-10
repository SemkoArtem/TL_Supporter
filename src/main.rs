#![cfg_attr(all(not(test), not(debug_assertions)), windows_subsystem = "windows")]
#![allow(dead_code)]

mod agent_core;
mod agent_ui;
mod audio;
mod capture;
mod devtest;
mod i18n;
mod lead_core;
mod lead_ui;
mod model;
mod net;
mod node;
mod proto;
mod secure;
mod secure_agent;
mod service;
mod setup_ui;
mod sysutil;
mod theme;
mod update;
mod util;

use crate::secure::Identity;
use anyhow::Result;
use eframe::egui;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

fn message(text: &str) {
    let (t, c) = (util::wide(text), util::wide("TL Supporter"));
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Returns false when this kind of window or program already runs in this session.
/// A mutex made by a copy with other rights cannot be opened, which also means it is running.
fn take_instance(kind: &str) -> bool {
    let name = format!("Local\\TLSupporter-{kind}{}", util::data_dir().to_string_lossy().replace(['\\', ':'], "_"));
    let wide_name = util::wide(&name);
    unsafe {
        let result = CreateMutexW(None, false, PCWSTR(wide_name.as_ptr()));
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        match result {
            Err(_) => false,
            Ok(handle) if exists => {
                let _ = CloseHandle(handle);
                false
            }
            // The handle stays open for the life of the process.
            Ok(_) => true,
        }
    }
}

/// Open the settings window, or bring it to the front when it is already open.
fn open_settings(identity: Identity) {
    if take_instance("settings-") {
        setup_ui::run_settings(identity);
    } else {
        agent_ui::restore_window(setup_ui::SETTINGS_TITLE);
    }
}

/// Exit quietly when the settings window asks for it.
fn watch_exit_flag() {
    std::thread::spawn(|| {
        let exit = util::data_dir().join(setup_ui::EXIT_FLAG);
        let restart = util::data_dir().join(setup_ui::RESTART_FLAG);
        let _ = std::fs::remove_file(&exit);
        let _ = std::fs::remove_file(&restart);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if exit.exists() {
                let request = std::fs::read(&exit).unwrap_or_default();
                let _ = std::fs::remove_file(&exit);
                if setup_ui::exit_request_valid(&request) {
                    // The service then leaves the program closed until the next sign-in.
                    std::process::exit(service::EXIT_STOPPED as i32);
                }
            }
            if restart.exists() {
                let _ = std::fs::remove_file(&restart);
                service::restart_program();
            }
        }
    });
}

fn run_app(identity: Identity) -> Result<()> {
    let state = model::load_state();
    let is_lead = state.console_mode(&identity.id);
    let title = state.directory.team_name.clone();
    let (ui_handle, rx) = agent_ui::UiHandle::new();
    let node = node::start(identity, state, ui_handle.clone())?;
    node.sync_employee_mode();
    node.ensure_admin_signature();
    update::discard_stale_update();
    node.console_started.store(if is_lead { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
    // The role may have changed while the network was starting.
    if node.state.read().console_mode(&node.identity.id) != is_lead {
        crate::service::restart_program();
    }
    watch_exit_flag();
    if is_lead {
        let ui_node = node.clone();
        std::thread::spawn(move || agent_ui::run(ui_node, rx, ui_handle, true));
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(agent_ui::CONSOLE_TITLE)
                .with_inner_size([980.0, 560.0])
                .with_min_inner_size([360.0, 120.0])
                .with_icon(egui::IconData { rgba: agent_ui::icon_rgba(), width: 32, height: 32 }),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        };
        let _ = title;
        eframe::run_native(agent_ui::CONSOLE_TITLE, options, Box::new(move |cc| Ok(Box::new(lead_ui::LeadApp::new(cc, node)))))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    } else {
        agent_ui::run(node, rx, ui_handle, false)
    }
}

fn real_main() -> Result<()> {
    i18n::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);

    if flag("--service") {
        return service::run_dispatcher();
    }
    if flag("--gen-release-key") {
        return update::generate_release_key(&args[1], &args[2]);
    }
    if flag("--sign-release") {
        return update::sign_release(&args[1], &args[2], &args[3]);
    }
    if flag("--uninstall") {
        return service::uninstall();
    }
    if flag("--install") {
        return service::install();
    }
    if flag("--clear-secure-pin") {
        return secure_agent::clear_pin();
    }

    if flag("--system-agent") {
        // The key is made by the user's program. This process only waits for it.
        let identity = loop {
            match Identity::load_existing() {
                Ok(identity) => break identity,
                Err(_) => std::thread::sleep(std::time::Duration::from_secs(5)),
            }
        };
        return secure_agent::run(identity);
    }
    let identity = Identity::load_or_create()?;
    if crate::util::test_env("TLS_DATA_DIR").is_some() {
        match args.first().map(String::as_str) {
            Some("--dev-create-team") => return devtest::create_team(&identity, &args[1], &args[2]),
            Some("--dev-e2e") => return devtest::run_e2e(identity, args[1].parse()?),
            Some("--dev-secure-e2e") => return devtest::run_secure_e2e(identity, &args[1]),
            Some("--dev-join") => return devtest::join(&identity, &args[1], &args[2]),
            Some("--dev-serve-code") => return devtest::serve_code(identity, args[1].parse()?),
            Some("--dev-employee-mode") => return devtest::employee_mode(&args[1]),
            Some("--dev-set-role") => return devtest::set_role(identity, &args[1]),
            Some("--dev-park") => return devtest::park(identity),
            Some("--dev-request-exit") => {
                setup_ui::request_exit(&args[1]);
                return Ok(());
            }
            Some("--dev-code-wait") => return devtest::code_wait(&identity, &args[1], &args[2]),
            Some("--dev-code-add") => return devtest::code_add(identity, &args[1]),
            _ => {}
        }
    }
    if flag("--settings") {
        open_settings(identity);
        return Ok(());
    }
    if flag("--exit-prompt") {
        if take_instance("exit-") {
            setup_ui::run_exit_prompt();
        } else {
            agent_ui::restore_window(setup_ui::EXIT_TITLE);
        }
        return Ok(());
    }
    let mut free = take_instance("");
    if !free && flag("--restarted") {
        // The previous copy is still closing.
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            free = take_instance("");
            if free {
                break;
            }
        }
    }
    if !free {
        // A launch by the service must stay invisible, otherwise it would open windows again and again.
        if flag("--from-service") {
            std::process::exit(service::EXIT_BUSY as i32);
        }
        // The program already runs here, so an extra launch only shows its window.
        if model::load_state().console_mode(&identity.id) {
            agent_ui::restore_console_window();
        } else {
            open_settings(identity);
        }
        return Ok(());
    }
    if model::load_state().directory.is_empty() {
        if !setup_ui::run_setup(identity.clone()) {
            // Closing the first-run window must not make the service open it again at once.
            if flag("--from-service") {
                std::process::exit(service::EXIT_STOPPED as i32);
            }
            return Ok(());
        }
        // A team is joined from a plain launch, so offer to install the autostart service.
        // If the prompt is declined the program still runs, just without autostart.
        if !service::is_installed()
            && !flag("--no-install")
            && crate::util::test_env("TLS_DATA_DIR").is_none()
            && service::relaunch_elevated("--install")
        {
            return Ok(());
        }
    }
    run_app(identity)
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(e) = real_main() {
        logi!("fatal: {e:#}");
        if !std::env::args().any(|a| a == "--service" || a == "--from-service") {
            message(&format!("{}: {e}", i18n::tr("Помилка запуску")));
        }
        std::process::exit(1);
    }
}
