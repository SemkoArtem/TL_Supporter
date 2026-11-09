//! First-run window and the simple settings window for employees.

use crate::model::{self, Directory, LocalState, MemberState, Role};
use crate::node;
use crate::secure::{self, Identity};
use crate::theme;
use eframe::egui::{self, Color32, RichText};

pub const EXIT_FLAG: &str = "exit.flag";
pub const RESTART_FLAG: &str = "restart.flag";
pub const SETTINGS_TITLE: &str = "Налаштування TL Supporter";

fn options(title: &str, w: f32, h: f32) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title(title).with_inner_size([w, h]).with_resizable(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Choose,
    Create,
    Join,
    Preset,
}

struct Setup {
    identity: Identity,
    mode: Mode,
    name: String,
    team: String,
    password: String,
    password2: String,
    code: String,
    preset_password: String,
    error: String,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Setup {
    fn finish(&mut self, state: LocalState, ctx: &egui::Context) {
        match model::save_state(&state) {
            Ok(()) => {
                self.done.store(true, std::sync::atomic::Ordering::SeqCst);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.error = format!("Не вдалося зберегти: {e}"),
        }
    }

    fn create(&mut self, ctx: &egui::Context) {
        if self.name.trim().is_empty() || self.team.trim().is_empty() {
            self.error = "Заповни ім'я і назву команди.".into();
            return;
        }
        if self.password.len() < 4 || self.password != self.password2 {
            self.error = "Паролі не збігаються або коротші за 4 символи.".into();
            return;
        }
        match Directory::new(self.team.trim(), &self.identity, self.name.trim(), secure::hash_password(&self.password)) {
            Ok(directory) => self.finish(LocalState { display_name: self.name.trim().to_string(), directory, ..Default::default() }, ctx),
            Err(e) => self.error = e.to_string(),
        }
    }

    fn join(&mut self, ctx: &egui::Context) {
        if self.name.trim().is_empty() || self.code.trim().is_empty() {
            self.error = "Заповни ім'я і код.".into();
            return;
        }
        match node::join_with_code(&self.identity, &self.code, self.name.trim()) {
            Ok(directory) => self.finish(LocalState { display_name: self.name.trim().to_string(), directory, ..Default::default() }, ctx),
            Err(e) => self.error = e.to_string(),
        }
    }

    fn preset(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new().add_filter("TL Supporter", &["tlpreset"]).pick_file() else { return };
        match state_from_preset(&self.identity, &path, &self.preset_password, self.name.trim()) {
            Ok(s) => self.finish(s, ctx),
            Err(e) => self.error = format!("Не вдалося відкрити пресет: {e}"),
        }
    }
}

/// Build this PC's state from a preset file. Also used by silent installation.
pub fn state_from_preset(identity: &Identity, path: &std::path::Path, password: &str, name: &str) -> anyhow::Result<LocalState> {
    let bytes = std::fs::read(path)?;
    let p = model::open_preset(password, &bytes)?;
    let display = if name.is_empty() {
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| identity.id.clone())
    } else {
        name.to_string()
    };
    let mut directory = p.directory;
    if p.role == Role::Lead {
        // The preset carries the team authority, so this PC can present proof that it is a lead.
        directory.members.insert(
            identity.id.clone(),
            model::Member {
                id: identity.id.clone(),
                public_b64: secure::b64(&identity.public),
                name: display.clone(),
                role: Role::Lead,
                state: MemberState::Approved,
                rev: model::Rev::new(&identity.id),
                ..Default::default()
            },
        );
    }
    Ok(LocalState { display_name: display, directory, lead: p.lead.unwrap_or_default(), ..Default::default() })
}

impl eframe::App for Setup {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("TL Supporter");
            ui.add_space(4.0);
            ui.weak(format!("Ключ цього ПК: {}", self.identity.id));
            ui.add_space(12.0);
            ui.label("Твоє ім'я, як його бачитимуть інші:");
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
            ui.add_space(10.0);
            match self.mode {
                Mode::Choose => {
                    if ui.button("Приєднатись за кодом від керівника").clicked() {
                        self.mode = Mode::Join;
                    }
                    if ui.button("Відкрити файл пресету").clicked() {
                        self.mode = Mode::Preset;
                    }
                    ui.add_space(10.0);
                    if ui.button("Створити нову команду (я керівник)").clicked() {
                        self.mode = Mode::Create;
                    }
                }
                Mode::Create => {
                    ui.label("Назва команди:");
                    ui.add(egui::TextEdit::singleline(&mut self.team).desired_width(f32::INFINITY));
                    ui.label("Адмінський пароль:");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(f32::INFINITY));
                    ui.label("Повтори пароль:");
                    ui.add(egui::TextEdit::singleline(&mut self.password2).password(true).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Створити").clicked() {
                            self.create(ctx);
                        }
                        if ui.button("Назад").clicked() {
                            self.mode = Mode::Choose;
                        }
                    });
                }
                Mode::Join => {
                    ui.label("Код від керівника:");
                    ui.add(egui::TextEdit::singleline(&mut self.code).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Приєднатись").clicked() {
                            self.join(ctx);
                        }
                        if ui.button("Назад").clicked() {
                            self.mode = Mode::Choose;
                        }
                    });
                }
                Mode::Preset => {
                    ui.label("Пароль файлу:");
                    ui.add(egui::TextEdit::singleline(&mut self.preset_password).password(true).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Вибрати файл…").clicked() {
                            self.preset(ctx);
                        }
                        if ui.button("Назад").clicked() {
                            self.mode = Mode::Choose;
                        }
                    });
                }
            }
            if !self.error.is_empty() {
                ui.add_space(8.0);
                ui.colored_label(Color32::from_rgb(232, 17, 35), &self.error);
            }
        });
    }
}

/// Returns true when this PC has joined a team.
pub fn run_setup(identity: Identity) -> bool {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let name = std::env::var("USERNAME").unwrap_or_default();
    let _ = eframe::run_native(
        "TL Supporter",
        options("TL Supporter", 460.0, 380.0),
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Setup {
                identity,
                mode: Mode::Choose,
                name,
                team: String::new(),
                password: String::new(),
                password2: String::new(),
                code: String::new(),
                preset_password: String::new(),
                error: String::new(),
                done: flag,
            }))
        }),
    );
    done.load(std::sync::atomic::Ordering::SeqCst)
}

struct Settings {
    id: String,
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
            ui.heading("TL Supporter");
            ui.add_space(8.0);
            egui::Grid::new("info").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                ui.label("Ім'я:");
                ui.strong(&me.name);
                ui.end_row();
                ui.label("Команда:");
                ui.label(&st.directory.team_name);
                ui.end_row();
                ui.label("Група:");
                ui.label(st.directory.group_name(&me.group));
                ui.end_row();
                ui.label("Ключ ПК:");
                ui.label(&self.id);
                ui.end_row();
                ui.label("Стан:");
                ui.label(match me.state {
                    MemberState::Approved => "Підключено до команди",
                    MemberState::Pending => "Очікує підтвердження керівника",
                    MemberState::Revoked => "Доступ скасовано",
                });
                ui.end_row();
                ui.label("Версія:");
                ui.label(crate::util::APP_VERSION);
                ui.end_row();
            });
            ui.add_space(14.0);
            ui.separator();
            if !self.unlocked {
                ui.label("Для зміни налаштувань потрібен адмінський пароль:");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(200.0));
                    if ui.button("Розблокувати").clicked() {
                        if secure_check(&st, &self.password) {
                            self.unlocked = true;
                            self.error.clear();
                        } else {
                            self.error = "Невірний пароль".into();
                        }
                        self.password.clear();
                    }
                });
                if !self.error.is_empty() {
                    ui.colored_label(Color32::from_rgb(232, 17, 35), &self.error);
                }
            } else {
                ui.label(RichText::new("Адмінський режим").strong());
                if st.role(&self.id) == Some(Role::Lead) && st.work_as_agent && ui.button("Повернутись у режим керівника").clicked() {
                    let mut fresh = model::load_state();
                    fresh.work_as_agent = false;
                    if model::save_state(&fresh).is_ok() {
                        let _ = std::fs::write(crate::util::data_dir().join(RESTART_FLAG), b"1");
                        self.message = "Зараз відкриється консоль керівника.".into();
                    }
                }
                if ui.button("Вийти з програми до наступного входу в Windows").clicked() {
                    let _ = std::fs::write(crate::util::data_dir().join(EXIT_FLAG), b"1");
                    self.message = "Програму буде закрито.".into();
                }
                if ui.button("Видалити цей ПК із програми (скинути)").clicked() {
                    let _ = model::save_state(&LocalState::default());
                    let _ = std::fs::write(crate::util::data_dir().join(EXIT_FLAG), b"1");
                    self.message = "Дані скинуто, програму буде закрито.".into();
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

pub fn run_settings(identity: Identity) {
    let _ = eframe::run_native(
        "TL Supporter",
        options(SETTINGS_TITLE, 460.0, 380.0),
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Settings { id: identity.id, password: String::new(), unlocked: false, error: String::new(), message: String::new() }))
        }),
    );
}
