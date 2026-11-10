//! First-run window and the simple settings window for employees.

use crate::i18n::tr;
use crate::model::{self, Directory, LocalState, MemberState, Role};
use crate::node;
use crate::secure::{self, Identity};
use crate::theme;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const EXIT_FLAG: &str = "exit.flag";
pub const RESTART_FLAG: &str = "restart.flag";
pub const SETTINGS_TITLE: &str = "TL Supporter Settings";
pub const EXIT_TITLE: &str = "Close TL Supporter";

/// What the exit request must contain. The data folder is writable by every user, so an empty
/// file must not be enough to close the program.
pub fn exit_token() -> Vec<u8> {
    let st = model::load_state();
    secure::sha256(format!("tlsupporter-exit:{}", st.directory.policy.admin_password).as_bytes())
}

/// Ask the running program to close. Call only after the admin password was checked.
fn request_exit() {
    let _ = std::fs::write(crate::util::data_dir().join(EXIT_FLAG), exit_token());
}

fn options(title: &str, w: f32, h: f32) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([w, h])
            .with_resizable(false)
            .with_icon(egui::IconData { rgba: crate::agent_ui::icon_rgba(), width: 32, height: 32 }),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Choose,
    Create,
}

struct Setup {
    identity: Identity,
    mode: Mode,
    name: String,
    team: String,
    password: String,
    password2: String,
    error: String,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    own_code: String,
    shown_name: Arc<Mutex<String>>,
    joined: Arc<Mutex<Option<Directory>>>,
}

/// Keep offering this PC to the network until a lead types its code.
fn wait_for_lead(identity: Identity, code: String, name: Arc<Mutex<String>>, joined: Arc<Mutex<Option<Directory>>>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let shown = name.lock().clone();
            if let Ok(directory) = node::try_join(&identity, &code, &shown, 1) {
                *joined.lock() = Some(directory);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}

impl Setup {
    fn finish(&mut self, state: LocalState, ctx: &egui::Context) {
        match model::save_state(&state) {
            Ok(()) => {
                self.done.store(true, std::sync::atomic::Ordering::SeqCst);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.error = format!("{}: {e}", tr("Не вдалося зберегти")),
        }
    }

    fn create(&mut self, ctx: &egui::Context) {
        if self.name.trim().is_empty() || self.team.trim().is_empty() {
            self.error = tr("Заповни ім'я і назву команди.").into();
            return;
        }
        if self.password.len() < 4 || self.password != self.password2 {
            self.error = tr("Паролі не збігаються або коротші за 4 символи.").into();
            return;
        }
        match Directory::new(self.team.trim(), &self.identity, self.name.trim(), secure::hash_password(&self.password)) {
            Ok(directory) => self.finish(LocalState { display_name: self.name.trim().to_string(), directory, ..Default::default() }, ctx),
            Err(e) => self.error = e.to_string(),
        }
    }

}

impl eframe::App for Setup {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(400));
        let typed = self.name.trim().to_string();
        let display = if typed.is_empty() { std::env::var("COMPUTERNAME").unwrap_or_else(|_| self.identity.id.clone()) } else { typed };
        *self.shown_name.lock() = display.clone();
        let joined = self.joined.lock().take();
        if let Some(directory) = joined {
            self.finish(LocalState { display_name: display, directory, ..Default::default() }, ctx);
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("TL Supporter");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    crate::i18n::language_picker(ui);
                });
            });
            ui.add_space(4.0);
            ui.weak(format!("{}: {}", tr("Ключ цього ПК"), self.identity.id));
            ui.add_space(12.0);
            ui.label(tr("Твоє ім'я, як його бачитимуть інші:"));
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
            ui.add_space(10.0);
            match self.mode {
                Mode::Choose => {
                    ui.label(tr("Код для підключення:"));
                    ui.label(RichText::new(&self.own_code).size(26.0).strong());
                    ui.weak(tr("Керівник вводить цей код на головному екрані. Чекаю на підключення…"));
                    ui.add_space(8.0);
                    if ui.button(tr("Створити нову команду (я керівник)")).clicked() {
                        self.mode = Mode::Create;
                    }
                }
                Mode::Create => {
                    ui.label(tr("Назва команди:"));
                    ui.add(egui::TextEdit::singleline(&mut self.team).desired_width(f32::INFINITY));
                    ui.label(tr("Адмінський пароль:"));
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(f32::INFINITY));
                    ui.label(tr("Повтори пароль:"));
                    ui.add(egui::TextEdit::singleline(&mut self.password2).password(true).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr("Створити")).clicked() {
                            self.create(ctx);
                        }
                        if ui.button(tr("Назад")).clicked() {
                            self.mode = Mode::Choose;
                        }
                    });
                }
            }
            if !self.error.is_empty() {
                ui.add_space(8.0);
                ui.colored_label(theme::danger(), &self.error);
            }
        });
    }
}

/// Returns true when this PC has joined a team.
pub fn run_setup(identity: Identity) -> bool {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let name = std::env::var("USERNAME").unwrap_or_default();
    let own_code = secure::generate_join_code();
    let shown_name = Arc::new(Mutex::new(name.clone()));
    let joined = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    wait_for_lead(identity.clone(), own_code.clone(), shown_name.clone(), joined.clone(), stop.clone());
    let (code_for_ui, name_for_ui, joined_for_ui) = (own_code, shown_name, joined);
    let _ = eframe::run_native(
        "TL Supporter",
        options("TL Supporter", 460.0, 440.0),
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, model::load_state().lead.light_theme);
            Ok(Box::new(Setup {
                identity,
                mode: Mode::Choose,
                name,
                team: String::new(),
                password: String::new(),
                password2: String::new(),
                error: String::new(),
                done: flag,
                own_code: code_for_ui,
                shown_name: name_for_ui,
                joined: joined_for_ui,
            }))
        }),
    );
    stop.store(true, Ordering::SeqCst);
    done.load(Ordering::SeqCst)
}

struct Settings {
    id: String,
    key: theme::KeyView,
    confirm_reset: bool,
    new_name: String,
    voice: f32,
    connect_sound: f32,
    password: String,
    unlocked: bool,
    error: String,
    message: String,
}

impl eframe::App for Settings {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        let st = model::load_state();
        let me = st.directory.member(&self.id).cloned().unwrap_or_default();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("TL Supporter");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // The running program notices the change by itself within a second.
                    crate::i18n::language_picker(ui);
                });
            });
            ui.add_space(8.0);
            egui::Grid::new("info").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Ім'я:"));
                ui.strong(&me.name);
                ui.end_row();
                ui.label(tr("Команда:"));
                ui.label(&st.directory.team_name);
                ui.end_row();
                ui.label(tr("Група:"));
                ui.label(tr(&st.directory.group_name(&me.group)));
                ui.end_row();
                ui.label(tr("Ключ ПК:"));
                ui.horizontal(|ui| self.key.show(ui, &self.id));
                ui.end_row();
                ui.label(tr("Стан:"));
                ui.label(match me.state {
                    MemberState::Approved => tr("Підключено до команди"),
                    MemberState::Pending => tr("Очікує підтвердження керівника"),
                    MemberState::Revoked => tr("Доступ скасовано"),
                });
                ui.end_row();
                ui.label(tr("Версія:"));
                ui.label(crate::util::APP_VERSION);
                ui.end_row();
            });
            ui.add_space(10.0);
            egui::Grid::new("sound").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Голос керівника"));
                let mut voice = self.voice * 100.0;
                let low = crate::audio::MIN_VOICE_VOLUME * 100.0;
                if ui.add(egui::Slider::new(&mut voice, low..=100.0).integer().suffix("%")).changed() {
                    self.voice = voice / 100.0;
                    crate::audio::save_voice_volume(self.voice);
                }
                ui.end_row();
                ui.label(tr("Звук підключення"));
                let mut chime = self.connect_sound * 100.0;
                if ui.add(egui::Slider::new(&mut chime, 0.0..=100.0).integer().suffix("%")).on_hover_text(tr("0% вимикає звук")).changed() {
                    self.connect_sound = chime / 100.0;
                    crate::audio::save_connect_volume(self.connect_sound);
                }
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.separator();
            if !self.unlocked {
                ui.label(tr("Для зміни налаштувань потрібен адмінський пароль:"));
                ui.horizontal(|ui| {
                    theme::field(ui, &mut self.password, "", true, 240.0);
                    if ui.button(tr("Розблокувати")).clicked() {
                        if secure_check(&st, &self.password) {
                            self.unlocked = true;
                            self.error.clear();
                        } else {
                            self.error = tr("Невірний пароль").into();
                        }
                        self.password.clear();
                    }
                });
                if !self.error.is_empty() {
                    ui.colored_label(theme::danger(), &self.error);
                }
            } else {
                ui.label(RichText::new(tr("Адмінський режим")).strong());
                ui.horizontal(|ui| {
                    ui.label(tr("Ім'я:"));
                    theme::field(ui, &mut self.new_name, "", false, 240.0);
                    if ui.button(tr("Зберегти")).clicked() && !self.new_name.trim().is_empty() {
                        let _ = std::fs::write(crate::util::data_dir().join(node::RENAME_FILE), self.new_name.trim());
                        self.message = tr("Ім'я зміниться, щойно керівник буде в мережі.").into();
                    }
                });
                if st.role(&self.id) == Some(Role::Lead) && st.work_as_agent && ui.button(tr("Повернутись у режим керівника")).clicked() {
                    let mut fresh = model::load_state();
                    fresh.work_as_agent = false;
                    if model::save_state(&fresh).is_ok() {
                        let _ = std::fs::write(crate::util::data_dir().join(RESTART_FLAG), b"1");
                        self.message = tr("Зараз відкриється консоль керівника.").into();
                    }
                }
                if ui.button(tr("Вийти з програми до наступного входу в Windows")).clicked() {
                    request_exit();
                    self.message = tr("Програму буде закрито.").into();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if self.confirm_reset {
                    ui.label(tr("Видалити команду і всі налаштування на цьому ПК?"));
                    ui.horizontal(|ui| {
                        if ui.button(tr("Так, скинути")).clicked() {
                            let _ = model::factory_reset();
                            let _ = std::fs::write(crate::util::data_dir().join(RESTART_FLAG), b"1");
                            self.confirm_reset = false;
                            self.message = tr("Дані скинуто, зараз відкриється початкове вікно.").into();
                        }
                        if ui.button(tr("Скасувати")).clicked() {
                            self.confirm_reset = false;
                        }
                    });
                } else if ui.button(tr("Скинути до заводських налаштувань")).clicked() {
                    self.confirm_reset = true;
                }
                if !self.message.is_empty() {
                    ui.label(&self.message);
                }
            }
        });
    }
}

fn secure_check(st: &LocalState, password: &str) -> bool {
    secure::check_password(&st.directory.policy.admin_password, password)
}

struct ExitPrompt {
    password: String,
    error: String,
    focused: bool,
}

impl eframe::App for ExitPrompt {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(tr("Щоб закрити програму, введи адмінський пароль команди:"));
            ui.add_space(6.0);
            let field = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(f32::INFINITY));
            if !self.focused {
                field.request_focus();
                self.focused = true;
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !self.error.is_empty() {
                ui.colored_label(theme::danger(), &self.error);
            }
            ui.add_space(8.0);
            let mut close = enter;
            ui.horizontal(|ui| {
                close |= ui.button(tr("Закрити програму")).clicked();
                if ui.button(tr("Скасувати")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            if close {
                if !self.password.is_empty() && secure_check(&model::load_state(), &self.password) {
                    request_exit();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.error = tr("Невірний пароль").into();
                    self.password.clear();
                    self.focused = false;
                }
            }
        });
    }
}

pub fn run_exit_prompt() {
    let _ = eframe::run_native(
        "TL Supporter",
        options(EXIT_TITLE, 400.0, 150.0),
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx, model::load_state().lead.light_theme);
            Ok(Box::new(ExitPrompt { password: String::new(), error: String::new(), focused: false }))
        }),
    );
}

pub fn run_settings(identity: Identity) {
    let _ = eframe::run_native(
        "TL Supporter",
        options(SETTINGS_TITLE, 480.0, 520.0),
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, model::load_state().lead.light_theme);
            let new_name = model::load_state().directory.member(&identity.id).map(|m| m.name.clone()).unwrap_or_default();
            Ok(Box::new(Settings { new_name, id: identity.id, key: Default::default(), confirm_reset: false, voice: crate::audio::load_voice_volume(), connect_sound: crate::audio::connect_volume(), password: String::new(), unlocked: false, error: String::new(), message: String::new() }))
        }),
    );
}
