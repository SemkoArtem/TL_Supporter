#![cfg_attr(all(not(test), not(debug_assertions)), windows_subsystem = "windows")]
#![allow(dead_code)]

mod agent_core;
mod agent_ui;
mod audio;
mod capture;
mod devtest;
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

use crate::model::Role;
use crate::secure::Identity;
use anyhow::Result;
use eframe::egui;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
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

/// Returns false when another copy already runs in this session.
fn single_instance() -> bool {
    let name = format!("Local\\TLSupporter-{}", util::data_dir().to_string_lossy().replace(['\\', ':'], "_"));
    let wide_name = util::wide(&name);
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(wide_name.as_ptr()));
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        std::mem::forget(handle);
        !exists
    }
}

/// Exit quietly when the settings window asks for it. The service then leaves the program closed.
fn watch_exit_flag() {
    std::thread::spawn(|| {
        let path = util::data_dir().join(setup_ui::EXIT_FLAG);
        let _ = std::fs::remove_file(&path);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if path.exists() {
                let _ = std::fs::remove_file(&path);
                std::process::exit(service::EXIT_STOPPED as i32);
            }
        }
    });
}

fn run_app(identity: Identity) -> Result<()> {
    let state = model::load_state();
    let role = state.role(&identity.id).unwrap_or(Role::Agent);
    let is_lead = role == Role::Lead;
    let title = state.directory.team_name.clone();
    let (ui_handle, rx) = agent_ui::UiHandle::new();
    let node = node::start(identity, state, ui_handle.clone())?;
    watch_exit_flag();
    if is_lead {
        let ui_node = node.clone();
        std::thread::spawn(move || agent_ui::run(ui_node, rx, ui_handle, true));
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(agent_ui::CONSOLE_TITLE)
                .with_inner_size([980.0, 560.0])
                .with_maximized(true)
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
        let preset = arg_value(&args, "--preset").map(|p| (p, arg_value(&args, "--preset-password").unwrap_or_default()));
        return service::install(preset);
    }

    let identity = Identity::load_or_create()?;
    if flag("--system-agent") {
        return secure_agent::run(identity);
    }
    if std::env::var("TLS_DATA_DIR").is_ok() {
        match args.first().map(String::as_str) {
            Some("--dev-create-team") => return devtest::create_team(&identity, &args[1], &args[2]),
            Some("--dev-export-preset") => return devtest::export_preset(&args[1], &args[2], args.get(3).is_some()),
            Some("--dev-import-preset") => return devtest::import_preset(&identity, &args[1], &args[2], &args[3]),
            Some("--dev-e2e") => return devtest::run_e2e(identity, args[1].parse()?),
            Some("--dev-secure-e2e") => return devtest::run_secure_e2e(identity, &args[1]),
            Some("--dev-join") => return devtest::join(&identity, &args[1], &args[2]),
            Some("--dev-serve-code") => return devtest::serve_code(identity, args[1].parse()?),
            _ => {}
        }
    }
    if flag("--settings") {
        setup_ui::run_settings(identity);
        return Ok(());
    }
    if !single_instance() {
        // The program already runs here, so an extra launch just opens the settings window.
        agent_ui::restore_console_window();
        if model::load_state().role(&identity.id) != Some(Role::Lead) {
            setup_ui::run_settings(identity);
        }
        return Ok(());
    }
    if model::load_state().directory.is_empty() {
        if !setup_ui::run_setup(identity.clone()) {
            return Ok(());
        }
        // A team is joined from a plain launch, so offer to install the autostart service.
        // If the prompt is declined the program still runs, just without autostart.
        if !service::is_installed()
            && !flag("--no-install")
            && std::env::var("TLS_DATA_DIR").is_err()
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
        if !std::env::args().any(|a| a == "--service") {
            message(&format!("Помилка запуску: {e}"));
        }
        std::process::exit(1);
    }
}
