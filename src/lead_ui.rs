//! Lead console: employee list or thumbnails, the live session view, journal and settings.

use crate::lead_core::{self, stroke_expired, MIC_BUTTON, MIC_LATCHED, MIC_OPEN};
use crate::model::{self, Group, MemberState, PhraseSet, Rev, Role};
use crate::node::Node;
use crate::proto::{DrawOp, InputEv, Mode, ScreenLevel, Tool};
use crate::secure;
use crate::sysutil;
use crate::theme::{self, icon};
use crate::util::{color_for, format_ts, now_ms};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Home,
    Session,
    Journal,
    Settings,
}

#[derive(PartialEq, Clone, Copy)]
enum SettingsTab {
    General,
    Sound,
    Drawing,
    Hints,
    Team,
}

const QUALITIES: [(u8, &str); 4] = [(0, "Максимальна"), (1, "Висока"), (2, "Середня"), (3, "Низька")];

struct ScreenTex {
    epoch: u64,
    handle: TextureHandle,
}

/// A helper thread is watching the auto-lock rules while the console is hidden.
static HIDDEN_WATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The console should show its lock screen the next time it opens.
static TRAY_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The next visible frame follows a pause in the tray.
static BACK_FROM_TRAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The window was closed to the tray, so the open sessions are gone when it returns.
static PASSIVE_RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Default)]
struct MemberDialog {
    id: String,
    name: String,
    group: String,
}

#[derive(Default)]
struct SettingsDraft {
    reasons: String,
    new_console_pw: String,
    new_admin_pw: String,
    confirm_employee_mode: bool,
    exit_pw: String,
    reset_pw: String,
    devices: Option<(Vec<(String, String)>, Vec<(String, String)>)>,
    add_code: String,
    add_group: String,
    new_group: String,
    new_set: String,
    phrase_edit: HashMap<String, String>,
    join_code: Option<String>,
    join_role_lead: bool,
    join_group: String,
    message: String,
    update_status: String,
    repo: String,
    personal: String,
}

pub struct LeadApp {
    node: Arc<Node>,
    page: Page,
    tab: SettingsTab,
    search: String,
    tabs: Vec<String>,
    active: Option<String>,
    textures: HashMap<String, ScreenTex>,
    thumbs: HashMap<String, (u64, TextureHandle)>,
    tool: Option<Tool>,
    drawing: Option<u32>,
    /// A frame or arrow being dragged. The employee sees it only after the button is released.
    preview: Option<(Tool, [f32; 2], [f32; 2])>,
    pointer_on: bool,
    pointer_last_ms: i64,
    pointer_last: [f32; 2],
    hints_open: bool,
    hint: String,
    control: bool,
    last_move: Pos2,
    modifiers: egui::Modifiers,
    member_dialog: Option<MemberDialog>,
    draft: SettingsDraft,
    locked: bool,
    unlock_input: String,
    nav_open: bool,
    nav_peek: bool,
    opened_maximized: bool,
    key_view: theme::KeyView,
    unlock_error: bool,
    last_tick: std::time::SystemTime,
    was_win_locked: bool,
    journal_filter: String,
    started: std::time::Instant,
}

/// Keep working in the tray. A hidden window gets no frames, so a helper thread watches
/// the auto-lock rules until the console is shown again.
fn send_to_tray(node: &Arc<Node>) {
    MIC_LATCHED.store(false, Ordering::Relaxed);
    MIC_BUTTON.store(false, Ordering::Relaxed);
    node.lead_set_thumbs(false);
    crate::agent_ui::hide_console_window();
    BACK_FROM_TRAY.store(true, Ordering::SeqCst);
    if HIDDEN_WATCH.swap(true, Ordering::SeqCst) {
        return;
    }
    let node = node.clone();
    std::thread::spawn(move || {
        let mut last = std::time::SystemTime::now();
        let mut was_locked = sysutil::is_desktop_locked();
        while !crate::agent_ui::console_visible() {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let now = std::time::SystemTime::now();
            let gap = now.duration_since(last).map(|d| d.as_secs()).unwrap_or(0);
            last = now;
            let s = node.state.read().lead.clone();
            let locked = sysutil::is_desktop_locked();
            let edge = locked && !was_locked;
            was_locked = locked;
            if (s.lock_with_windows && edge) || (s.lock_on_sleep && gap > 30) || (s.lock_enabled && sysutil::idle_ms() > s.lock_idle_min.max(1) as u64 * 60_000) {
                TRAY_LOCK.store(true, Ordering::SeqCst);
                node.lead_lock_console();
            }
        }
        HIDDEN_WATCH.store(false, Ordering::SeqCst);
    });
}

impl LeadApp {
    pub fn new(cc: &eframe::CreationContext<'_>, node: Arc<Node>) -> Self {
        theme::apply(&cc.egui_ctx, node.state.read().lead.light_theme);
        *node.repaint.lock() = Some(cc.egui_ctx.clone());
        // A minimized window gets no frames either, so this cannot be checked in `update`.
        let watcher = node.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if crate::agent_ui::console_wants_tray() {
                send_to_tray(&watcher);
            }
        });
        let (reasons, repo, personal) = {
            let st = node.state.read();
            (st.directory.policy.help_reasons.join("\n"), st.directory.policy.update_repo.clone(), st.lead.personal_phrases.join("\n"))
        };
        Self {
            node,
            page: Page::Home,
            tab: SettingsTab::General,
            search: String::new(),
            tabs: Vec::new(),
            active: None,
            textures: HashMap::new(),
            thumbs: HashMap::new(),
            tool: None,
            drawing: None,
            preview: None,
            pointer_on: false,
            pointer_last_ms: 0,
            pointer_last: [0.0, 0.0],
            hints_open: false,
            hint: String::new(),
            control: false,
            last_move: Pos2::ZERO,
            modifiers: egui::Modifiers::NONE,
            member_dialog: None,
            draft: SettingsDraft { reasons, repo, personal, ..Default::default() },
            locked: false,
            unlock_input: String::new(),
            nav_open: true,
            nav_peek: false,
            opened_maximized: false,
            key_view: Default::default(),
            unlock_error: false,
            last_tick: std::time::SystemTime::now(),
            was_win_locked: false,
            journal_filter: String::new(),
            started: std::time::Instant::now(),
        }
    }

    fn save_lead(&self, f: impl FnOnce(&mut model::LeadSettings)) {
        f(&mut self.node.state.write().lead);
        self.node.save();
    }

    fn has_lock_password(&self) -> bool {
        let st = self.node.state.read();
        !st.lead.console_password.is_empty() || !st.directory.policy.admin_password.is_empty()
    }

    fn lock_now(&mut self) {
        if self.locked || !self.has_lock_password() {
            return;
        }
        self.locked = true;
        self.unlock_input.clear();
        self.unlock_error = false;
        self.control = false;
        self.drawing = None;
        self.node.lead_lock_console();
        self.node.audit_log("—", "Консоль заблоковано");
    }

    /// The close button ends the sessions too, the minimize button keeps them.
    fn hide_to_tray(&mut self) {
        self.node.lead_passive();
        PASSIVE_RESET.store(true, Ordering::SeqCst);
        send_to_tray(&self.node);
    }

    fn check_auto_lock(&mut self) {
        if TRAY_LOCK.swap(false, Ordering::SeqCst) {
            self.lock_now();
        }
        let now = std::time::SystemTime::now();
        let mut gap = now.duration_since(self.last_tick).map(|d| d.as_secs()).unwrap_or(0);
        self.last_tick = now;
        if BACK_FROM_TRAY.load(Ordering::SeqCst) && crate::agent_ui::console_visible() {
            // The pause came from hiding the window, not from the PC sleeping.
            BACK_FROM_TRAY.store(false, Ordering::SeqCst);
            gap = 0;
            self.was_win_locked = sysutil::is_desktop_locked();
        }
        let s = self.node.state.read().lead.clone();
        let win_locked = sysutil::is_desktop_locked();
        let win_edge = win_locked && !self.was_win_locked;
        self.was_win_locked = win_locked;
        if s.lock_with_windows && win_edge {
            self.lock_now();
        }
        if s.lock_on_sleep && gap > 30 {
            self.lock_now();
        }
        if s.lock_enabled && sysutil::idle_ms() > s.lock_idle_min.max(1) as u64 * 60_000 {
            self.lock_now();
        }
    }

    fn try_unlock(&mut self) {
        let (console, admin) = {
            let st = self.node.state.read();
            (st.lead.console_password.clone(), st.directory.policy.admin_password.clone())
        };
        let ok = if console.is_empty() { secure::check_password(&admin, &self.unlock_input) } else { secure::check_password(&console, &self.unlock_input) };
        if ok {
            self.locked = false;
            self.unlock_input.clear();
            self.node.lead_unlock_console();
            self.node.audit_log("—", "Консоль розблоковано");
        } else {
            self.unlock_error = true;
        }
    }

    fn lock_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.label(RichText::new(icon::LOCK).size(40.0));
                ui.add_space(8.0);
                ui.heading("Консоль заблоковано");
                ui.add_space(12.0);
                let r = ui.add(egui::TextEdit::singleline(&mut self.unlock_input).password(true).desired_width(240.0).hint_text("Пароль"));
                r.request_focus();
                if self.unlock_error {
                    ui.colored_label(theme::danger(), "Невірний пароль");
                }
                ui.add_space(8.0);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Розблокувати").clicked() || enter {
                    self.try_unlock();
                }
            });
        });
    }

    fn open_session(&mut self, id: &str, mode: Option<Mode>) {
        if !self.tabs.iter().any(|t| t == id) {
            self.tabs.push(id.to_string());
        }
        self.select_tab(id);
        if let Some(m) = mode {
            self.node.lead_set_mode(id, m);
        }
    }

    fn select_tab(&mut self, id: &str) {
        self.active = Some(id.to_string());
        self.page = Page::Session;
        self.tool = None;
        self.control = false;
        self.node.lead_set_voice_target(Some(id.to_string()));
    }

    fn close_tab(&mut self, id: &str) {
        self.node.lead_set_mode(id, Mode::default());
        self.tabs.retain(|t| t != id);
        self.textures.remove(id);
        if self.active.as_deref() == Some(id) {
            self.active = self.tabs.last().cloned();
            match self.active.clone() {
                Some(a) => self.select_tab(&a),
                None => {
                    self.page = Page::Home;
                    self.node.lead_set_voice_target(None);
                }
            }
        }
    }

    fn dev_auto_session(&mut self) {
        if std::env::var("TLS_DATA_DIR").is_err() || std::env::var("TLS_AUTO_SESSION").is_err() || !self.tabs.is_empty() {
            return;
        }
        if self.last_tick.elapsed().is_err() {
            return;
        }
        let online = self.node.peers.lock().keys().next().cloned();
        if let (Some(id), true) = (online, self.started.elapsed().as_secs() >= 6) {
            self.open_session(&id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0 }));
        }
    }

    /// Left navigation pane. It folds to icons on pages that need the width, and the
    /// menu button opens it there for a moment.
    fn nav_rail(&mut self, ctx: &egui::Context) {
        let folds = matches!(self.page, Page::Session | Page::Settings);
        let expanded = if folds { self.nav_peek } else { self.nav_open };
        egui::SidePanel::left("nav")
            .exact_width(if expanded { 240.0 } else { 48.0 })
            .resizable(false)
            .show_separator_line(false)
            .frame(theme::rail_frame())
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if theme::nav_item(ui, icon::MENU, "", false, expanded).clicked() {
                    if folds {
                        self.nav_peek = !self.nav_peek;
                    } else {
                        self.nav_open = !self.nav_open;
                    }
                }
                let mut go = None;
                if theme::nav_item(ui, icon::PEOPLE, "Працівники", self.page == Page::Home, expanded).clicked() {
                    go = Some(Page::Home);
                }
                let mut close = None;
                for id in self.tabs.clone() {
                    let selected = self.page == Page::Session && self.active.as_deref() == Some(&id);
                    let name = self.node.member_name(&id);
                    // Folded rows show initials so that several sessions can be told apart.
                    let initials = name.chars().take(2).collect::<String>().to_uppercase();
                    let glyph = if expanded { icon::MONITOR } else { initials.as_str() };
                    let row = theme::nav_item(ui, glyph, &name, selected, expanded);
                    if row.clicked() {
                        self.select_tab(&id);
                        self.nav_peek = false;
                    }
                    row.context_menu(|ui| {
                        if ui.button("Завершити сеанс").clicked() {
                            close = Some(id.clone());
                            ui.close_menu();
                        }
                    });
                    if expanded {
                        let spot = Rect::from_center_size(Pos2::new(row.rect.right() - 22.0, row.rect.center().y), Vec2::splat(28.0));
                        if ui.put(spot, egui::Button::new(icon::CLOSE).frame(false)).on_hover_text("Завершити сеанс").clicked() {
                            close = Some(id.clone());
                        }
                    }
                }
                if let Some(id) = close {
                    self.close_tab(&id);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    let lock_hint = if self.has_lock_password() { "Заблокувати" } else { "Спочатку задай пароль" };
                    if theme::nav_item(ui, icon::LOCK, lock_hint, false, expanded).clicked() {
                        self.lock_now();
                    }
                    if theme::nav_item(ui, icon::SETTINGS, "Налаштування", self.page == Page::Settings, expanded).clicked() {
                        go = Some(Page::Settings);
                    }
                    if theme::nav_item(ui, icon::HISTORY, "Журнал", self.page == Page::Journal, expanded).clicked() {
                        go = Some(Page::Journal);
                    }
                    let quiet = self.node.state.read().lead.do_not_disturb;
                    if theme::nav_item(ui, icon::QUIET, if quiet { "Не турбувати: увімкнено" } else { "Не турбувати" }, quiet, expanded).clicked() {
                        self.save_lead(|l| l.do_not_disturb = !quiet);
                        self.node.ui.send(crate::agent_ui::UiCmd::Quiet(!quiet));
                    }
                });
                if let Some(page) = go {
                    self.page = page;
                    self.nav_peek = false;
                }
            });
    }

    fn mic_controls(&mut self, ui: &mut egui::Ui) {
        let (client_mode, can_client) = (self.node.state.read().lead.mic_mode_client, false);
        ui.label("Чує:");
        if theme::pick(ui, !client_mode || !can_client, "Лише працівник").clicked() {
            self.save_lead(|l| l.mic_mode_client = false);
        }
        ui.add_enabled(can_client, egui::Button::new("І клієнт"))
            .on_disabled_hover_text("Потрібен драйвер віртуального мікрофона. Буде додано пізніше.");
        ui.separator();
        let latched = MIC_LATCHED.load(Ordering::Relaxed);
        let open = MIC_OPEN.load(Ordering::Relaxed);
        let text = format!("{} {}", if latched { icon::MIC } else { icon::MIC_OFF }, if latched { "Мікрофон увімкнено" } else { "Мікрофон вимкнено" });
        let mut b = egui::Button::new(text);
        if latched {
            b = b.fill(theme::ACCENT).stroke(Stroke::NONE);
        }
        if ui.add(b).clicked() {
            MIC_LATCHED.fetch_xor(true, Ordering::Relaxed);
        }
        let hold = ui.add(egui::Button::new(format!("{} Утримуй", icon::MIC)).sense(Sense::click_and_drag()));
        MIC_BUTTON.store(hold.is_pointer_button_down_on(), Ordering::Relaxed);
        ui.add(egui::ProgressBar::new(crate::audio::mic_level().min(1.0)).desired_width(70.0).rounding(0.0));
        let mut gain = self.node.state.read().lead.mic_gain * 100.0;
        ui.spacing_mut().slider_width = 80.0;
        ui.label("Гучність");
        if ui.add(egui::Slider::new(&mut gain, 20.0..=200.0).integer().suffix("%")).on_hover_text("Наскільки голосно тебе чує працівник").changed() {
            let value = gain / 100.0;
            crate::audio::set_mic_gain(value);
            self.save_lead(|l| l.mic_gain = value);
        }
        if crate::audio::mic_failed() {
            ui.colored_label(theme::danger(), "Мікрофон не відкрився").on_hover_text("Перевір пристрій у Налаштування → Звук");
        } else if latched && now_ms() - crate::audio::mic_last_sound() > 3000 {
            ui.colored_label(Color32::from_rgb(202, 130, 16), "Немає сигналу").on_hover_text("Мікрофон вимкнений у Windows або вибрано не той пристрій. Налаштування → Звук");
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        if open {
            ui.colored_label(theme::danger(), RichText::new("● В ефірі").strong());
        }
    }

    fn phrases(&self) -> Vec<(String, Vec<String>)> {
        let st = self.node.state.read();
        let mut out: Vec<(String, Vec<String>)> =
            st.directory.phrases.values().filter(|p| !p.deleted).map(|p| (p.name.clone(), p.phrases.clone())).collect();
        if !st.lead.personal_phrases.is_empty() {
            out.insert(0, ("Мої".to_string(), st.lead.personal_phrases.clone()));
        }
        out
    }

    fn hint_bar(&mut self, ui: &mut egui::Ui) {
        let Some(target) = self.active.clone() else { return };
        let phrases = self.phrases();
        ui.menu_button("Фрази", |ui| {
            for (set, list) in phrases {
                ui.menu_button(set, |ui| {
                    for p in list {
                        if ui.button(&p).clicked() {
                            self.node.lead_send_hint(&target, p.clone());
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        let total = self.node.lead.lock().targets.get(&target).map(|t| t.hints.len()).unwrap_or(0);
        if theme::pick(ui, self.hints_open, format!("Історія ({total})")).clicked() {
            self.hints_open = !self.hints_open;
        }
        let w = (ui.available_width() - 120.0).max(60.0);
        let r = ui.add(egui::TextEdit::singleline(&mut self.hint).desired_width(w).hint_text("Текст підказки"));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button(format!("{} Надіслати", icon::SEND)).clicked() || enter) && !self.hint.trim().is_empty() {
            let text = std::mem::take(&mut self.hint);
            self.node.lead_send_hint(&target, text);
        }
    }

    fn help_banner(&mut self, ctx: &egui::Context) {
        let items = self.node.lead.lock().help.clone();
        if items.is_empty() {
            return;
        }
        egui::TopBottomPanel::top("help").frame(egui::Frame::none().fill(theme::warn_fill()).inner_margin(6.0)).show(ctx, |ui| {
            for h in items {
                ui.horizontal(|ui| {
                    let name = self.node.member_name(&h.target);
                    let reason = if h.reason.is_empty() { String::new() } else { format!(" — {}", h.reason) };
                    let missed = h.is_missed(now_ms());
                    if missed {
                        ui.colored_label(theme::danger(), icon::MISSED).on_hover_text("Ніхто не взяв цей запит");
                        ui.strong(format!("{name}: пропущений запит{reason}"));
                        ui.weak(format_ts(h.ts));
                    } else {
                        ui.strong(format!("{} {name}: потрібна допомога{reason}", icon::HELP));
                    }
                    if h.taken_by.is_empty() {
                        if ui.button(if missed { "Підключитись" } else { "Взяти" }).clicked() {
                            self.node.lead_take_help(&h.target, h.id);
                            self.open_session(&h.target, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0 }));
                        }
                    } else {
                        ui.label(format!("Узяв(ла): {}", h.taken_by));
                    }
                    if ui.small_button(icon::CLOSE).clicked() {
                        self.node.lead_dismiss_help(&h.target, h.id);
                    }
                });
            }
        });
    }

    fn home(&mut self, ctx: &egui::Context) {
        let grid_view = self.node.state.read().lead.grid_view;
        self.node.lead_set_thumbs(grid_view);
        egui::CentralPanel::default().frame(theme::page_frame(ctx)).show(ctx, |ui| {
            ui.heading("Працівники");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Ключ цього ПК:");
                let key = self.node.identity.id.clone();
                self.key_view.show(ui, &key);
            });
            self.add_by_code(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(240.0).hint_text("Пошук за іменем"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::pick(ui, grid_view, format!("{}  Мініатюри", icon::GRID)).clicked() {
                        self.save_lead(|l| l.grid_view = true);
                    }
                    if theme::pick(ui, !grid_view, format!("{}  Список", icon::LIST)).clicked() {
                        self.save_lead(|l| l.grid_view = false);
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                self.pending_section(ui);
                let groups = self.visible_groups();
                if groups.is_empty() {
                    ui.weak("Поки немає жодного ПК. Запусти програму на новому ПК, вона покаже код. Введи його вище.");
                }
                for (gid, gname, members) in groups {
                    let online = members.iter().filter(|m| self.node.peer(m).is_some()).count();
                    egui::CollapsingHeader::new(RichText::new(format!("{gname}   {online}/{}", members.len())).strong())
                        .id_salt(&gid)
                        .default_open(true)
                        .show(ui, |ui| {
                            if grid_view {
                                self.grid(ui, ctx, &members);
                            } else {
                                for m in &members {
                                    self.row(ui, m);
                                }
                            }
                        });
                }
            });
        });
    }

    fn visible_groups(&self) -> Vec<(String, String, Vec<String>)> {
        let st = self.node.state.read();
        let needle = self.search.to_lowercase();
        let d = &st.directory;
        let me = &self.node.identity.id;
        let mut map: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for m in d.members.values() {
            if m.state != MemberState::Approved || &m.id == me || !d.lead_can_reach(me, &m.id) {
                continue;
            }
            if !needle.is_empty() && !m.name.to_lowercase().contains(&needle) {
                continue;
            }
            map.entry(m.group.clone()).or_default().push(m.id.clone());
        }
        let mut out: Vec<(String, String, Vec<String>)> = map
            .into_iter()
            .map(|(g, mut v)| {
                v.sort_by_key(|id| d.member(id).map(|m| m.name.to_lowercase()).unwrap_or_default());
                let name = d.group_name(&g);
                (g, name, v)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// Add a new PC by the code it shows: find it first, then choose its group.
    fn add_by_code(&mut self, ui: &mut egui::Ui) {
        let check = self.node.lead_check_view();
        ui.horizontal(|ui| {
            ui.label("Додати ПК за кодом:");
            let field = ui.add(egui::TextEdit::singleline(&mut self.draft.add_code).desired_width(170.0).hint_text("Код із нового ПК"));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add(theme::accent_button("Перевірити")).clicked() || enter) && !self.draft.add_code.trim().is_empty() {
                self.node.lead_check_code(&self.draft.add_code);
                self.draft.add_group = self.group_options().get(1).map(|g| g.0.clone()).unwrap_or_default();
            }
        });
        let Some(c) = check else { return };
        let opts = self.group_options();
        let group_name = |id: &str| opts.iter().find(|o| o.0 == id).map(|o| o.1.clone()).unwrap_or_default();
        if c.added {
            ui.horizontal(|ui| {
                ui.label(format!("{} додано в групу «{}».", c.found_name, group_name(c.group.as_deref().unwrap_or(""))));
                if ui.button("Готово").clicked() {
                    self.node.lead_check_cancel();
                    self.draft.add_code.clear();
                }
            });
        } else if c.group.is_some() {
            ui.weak(format!("Додаю {}…", c.found_name));
        } else if !c.found_id.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Знайдено:");
                ui.strong(&c.found_name);
                ui.weak(&c.found_id);
                let current = group_name(&self.draft.add_group);
                egui::ComboBox::from_id_salt("add_group").selected_text(current).show_ui(ui, |ui| {
                    for (id, name) in &opts {
                        ui.selectable_value(&mut self.draft.add_group, id.clone(), name);
                    }
                });
                if ui.add(theme::accent_button("Додати в групу")).clicked() {
                    self.node.lead_add_checked(self.draft.add_group.clone());
                }
                if ui.button("Скасувати").clicked() {
                    self.node.lead_check_cancel();
                }
            });
        } else if c.expires_ms < now_ms() {
            ui.horizontal(|ui| {
                ui.colored_label(theme::danger(), "ПК з таким кодом не знайдено. Перевір код і чи відкрито на тому ПК початкове вікно програми.");
                if ui.small_button(icon::CLOSE).clicked() {
                    self.node.lead_check_cancel();
                }
            });
        } else {
            ui.weak("Шукаю ПК з цим кодом…");
        }
    }

    fn pending_section(&mut self, ui: &mut egui::Ui) {
        let pending: Vec<model::Member> = self.node.state.read().directory.members.values().filter(|m| m.state == MemberState::Pending).cloned().collect();
        if pending.is_empty() {
            return;
        }
        ui.strong("Очікують підтвердження");
        for m in pending {
            ui.horizontal(|ui| {
                ui.label(format!("{}   {}", m.name, m.id));
                if ui.button("Підтвердити").clicked() {
                    self.member_dialog = Some(MemberDialog { id: m.id.clone(), name: m.name.clone(), group: m.group.clone() });
                    let (id, name) = (m.id.clone(), m.name.clone());
                    self.node.update_directory(|d, me| {
                        if let Some(x) = d.members.get_mut(&id) {
                            x.state = MemberState::Approved;
                            x.rev = Rev::new(me);
                        }
                    });
                    self.node.audit_log(&name, "ПК підтверджено");
                }
                if ui.button("Відхилити").clicked() {
                    self.revoke(&m.id, &m.name);
                }
            });
        }
        ui.separator();
    }

    fn revoke(&self, id: &str, name: &str) {
        let target = id.to_string();
        self.node.update_directory(|d, me| {
            if let Some(x) = d.members.get_mut(&target) {
                x.state = MemberState::Revoked;
                x.rev = Rev::new(me);
            }
        });
        self.node.audit_log(name, "Доступ ПК скасовано");
    }

    fn status_dot(&self, ui: &mut egui::Ui, id: &str, locked: bool) {
        let online = self.node.is_reachable(id);
        let c = if !online {
            Color32::from_gray(170)
        } else if locked || self.node.peer(id).is_none() {
            Color32::from_rgb(202, 130, 16)
        } else {
            Color32::from_rgb(16, 137, 62)
        };
        let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 5.0, c);
    }

    fn row(&mut self, ui: &mut egui::Ui, id: &str) {
        let name = self.node.member_name(id);
        let (mode, locked) = {
            let l = self.node.lead.lock();
            l.targets.get(id).map(|t| (t.mode, t.locked)).unwrap_or((Mode::default(), false))
        };
        let online = self.node.is_reachable(id);
        let sign_in_screen = online && self.node.peer(id).is_none();
        let backdrop = ui.painter().add(egui::Shape::Noop);
        let line = ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            self.status_dot(ui, id, locked);
            let r = ui
                .allocate_ui_with_layout(Vec2::new(240.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_width(240.0);
                    ui.add(egui::Label::new(&name).truncate().sense(Sense::click()))
                })
                .inner;
            r.context_menu(|ui| self.member_menu(ui, id, &name));
            if self.node.lead.lock().help.iter().any(|h| h.target == id && h.is_missed(now_ms())) {
                ui.colored_label(theme::danger(), icon::MISSED).on_hover_text("Пропущений запит допомоги");
            }
            if self.node.state.read().lead.muted_help.iter().any(|m| m == id) {
                ui.weak(icon::QUIET).on_hover_text("Запити допомоги з цього ПК вимкнено");
            }
            if sign_in_screen {
                ui.weak("Екран входу");
            } else if locked {
                ui.weak("Windows заблоковано");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(online, |ui| {
                    if ui.add(theme::accent_button("Підключитись")).clicked() {
                        self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0 }));
                    }
                    if theme::pick(ui, mode.listen && mode.screen == ScreenLevel::Off, format!("{} Лише слухати", icon::HEADPHONE)).clicked() {
                        self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Off, monitor: 0, quality: 0 }));
                    }
                });
                if ui.small_button(icon::MORE).clicked() {
                    self.member_dialog = Some(self.dialog_for(id));
                }
            });
        });
        let area = line.response.rect.expand2(Vec2::new(6.0, 1.0));
        if ui.rect_contains_pointer(area) {
            ui.painter().set(backdrop, egui::Shape::rect_filled(area, 0.0, theme::hover_fill()));
        }
    }

    fn dialog_for(&self, id: &str) -> MemberDialog {
        let st = self.node.state.read();
        let m = st.directory.member(id).cloned().unwrap_or_default();
        MemberDialog { id: id.to_string(), name: m.name, group: m.group }
    }

    fn member_menu(&mut self, ui: &mut egui::Ui, id: &str, _name: &str) {
        if ui.button("Перейменувати або перемістити…").clicked() {
            self.member_dialog = Some(self.dialog_for(id));
            ui.close_menu();
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, members: &[String]) {
        ui.horizontal_wrapped(|ui| {
            for id in members {
                self.update_thumb(ctx, id);
                let name = self.node.member_name(id);
                let online = self.node.peer(id).is_some();
                let frame = egui::Frame::none().stroke(Stroke::new(1.0_f32, theme::border())).inner_margin(4.0);
                frame.show(ui, |ui| {
                    ui.set_width(250.0);
                    ui.vertical(|ui| {
                        let size = Vec2::new(250.0, 140.0);
                        let (rect, r) = ui.allocate_exact_size(size, Sense::click());
                        ui.painter().rect_filled(rect, 0.0, Color32::from_gray(40));
                        if let Some((_, tex)) = self.thumbs.get(id) {
                            let t = tex.size_vec2();
                            let scale = (size.x / t.x).min(size.y / t.y);
                            let img = Rect::from_center_size(rect.center(), t * scale);
                            ui.painter().image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        } else {
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, if online { "Завантаження…" } else { "Не в мережі" }, egui::FontId::proportional(13.0), Color32::from_gray(180));
                        }
                        if r.clicked() && online {
                            self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0 }));
                        }
                        r.context_menu(|ui| self.member_menu(ui, id, &name));
                        ui.horizontal(|ui| {
                            self.status_dot(ui, id, false);
                            ui.add(egui::Label::new(&name).truncate());
                        });
                        let reachable = self.node.is_reachable(id);
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(reachable, |ui| {
                                if ui.add(theme::accent_button("Підключитись")).clicked() {
                                    self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0 }));
                                }
                                if ui.button(icon::HEADPHONE).on_hover_text("Лише слухати").clicked() {
                                    self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Off, monitor: 0, quality: 0 }));
                                }
                            });
                            if ui.button(icon::MORE).on_hover_text("Ім'я, група, запити").clicked() {
                                self.member_dialog = Some(self.dialog_for(id));
                            }
                            if self.node.lead.lock().help.iter().any(|h| h.target == id.as_str() && h.is_missed(now_ms())) {
                                ui.colored_label(theme::danger(), icon::MISSED).on_hover_text("Пропущений запит допомоги");
                            }
                        });
                    });
                });
            }
        });
    }

    fn update_thumb(&mut self, ctx: &egui::Context, id: &str) {
        let l = self.node.lead.lock();
        let Some(t) = l.targets.get(id) else { return };
        let Some((w, h, rgba)) = &t.thumb else { return };
        if self.thumbs.get(id).map(|x| x.0) == Some(t.thumb_rev) {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([*w, *h], rgba);
        let handle = ctx.load_texture(format!("thumb-{id}"), image, TextureOptions::LINEAR);
        self.thumbs.insert(id.to_string(), (t.thumb_rev, handle));
    }

    fn update_screen_texture(&mut self, ctx: &egui::Context, id: &str) {
        let mut l = self.node.lead.lock();
        let Some(t) = l.targets.get_mut(id) else { return };
        let (w, h, epoch) = (t.screen.width as usize, t.screen.height as usize, t.screen.epoch);
        if w == 0 || h == 0 {
            self.textures.remove(id);
            return;
        }
        let rebuild = self.textures.get(id).map(|x| x.epoch != epoch).unwrap_or(true);
        if rebuild {
            let blank = egui::ColorImage::new([w, h], Color32::from_gray(30));
            let handle = ctx.load_texture(format!("screen-{id}"), blank, TextureOptions::LINEAR);
            self.textures.insert(id.to_string(), ScreenTex { epoch, handle });
        }
        let tex = self.textures.get_mut(id).unwrap();
        for ((x, y), tile) in t.screen.tiles.drain() {
            if x as usize + tile.w > w || y as usize + tile.h > h {
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([tile.w, tile.h], &tile.rgba);
            tex.handle.set_partial([x as usize, y as usize], image, TextureOptions::LINEAR);
        }
    }

    fn session(&mut self, ctx: &egui::Context) {
        let Some(id) = self.active.clone() else {
            self.page = Page::Home;
            return;
        };
        self.update_screen_texture(ctx, &id);
        let (mode, monitors, locked, activity, holder, cursor, secure_holder) = {
            let l = self.node.lead.lock();
            match l.targets.get(&id) {
                Some(t) => (t.mode, t.monitors.clone(), t.locked, t.activity.clone(), t.control_holder.clone(), t.cursor, t.secure_holder.clone()),
                None => (Mode::default(), vec![], false, vec![], String::new(), None, String::new()),
            }
        };
        let online = self.node.is_reachable(&id);
        let (consent, last_audio, history) = {
            let l = self.node.lead.lock();
            l.targets.get(&id).map(|t| (t.consent, t.last_audio_ms, t.hints.clone())).unwrap_or((0, 0, Vec::new()))
        };
        let on_secure_screen = locked || self.node.peer(&id).is_none();
        let has_secure_link = self.node.secure_peer(&id).is_some();
        let me = self.node.identity.id.clone();
        let rights = self.node.state.read().directory.member(&me).map(|m| m.rights.clone()).unwrap_or_default();
        let i_control = holder == me || secure_holder == me;
        self.control = i_control;

        egui::TopBottomPanel::top("session_tools").frame(theme::bar_frame()).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(format!("{} Завершити", icon::CLOSE)).on_hover_text("Завершити сеанс").clicked() {
                    self.close_tab(&id);
                }
                ui.separator();
                let mut next = mode;
                if theme::pick(ui, mode.listen, format!("{} Слухати", icon::HEADPHONE)).clicked() {
                    next.listen = !mode.listen;
                }
                ui.separator();
                let current = if mode.screen == ScreenLevel::Off { "Вимкнено" } else { QUALITIES[mode.quality.min(3) as usize].1 };
                ui.label("Екран:");
                egui::ComboBox::from_id_salt("quality").selected_text(current).show_ui(ui, |ui| {
                    if ui.add(egui::SelectableLabel::new(mode.screen == ScreenLevel::Off, "Вимкнено")).clicked() {
                        next.screen = ScreenLevel::Off;
                    }
                    for (q, label) in QUALITIES {
                        if ui.add(egui::SelectableLabel::new(mode.screen != ScreenLevel::Off && mode.quality == q, label)).clicked() {
                            next.screen = ScreenLevel::Full;
                            next.quality = q;
                        }
                    }
                });
                if monitors.len() > 1 {
                    egui::ComboBox::from_id_salt("mon").selected_text(format!("Монітор {}", mode.monitor + 1)).show_ui(ui, |ui| {
                        for m in &monitors {
                            ui.selectable_value(&mut next.monitor, m.index, format!("Монітор {}", m.index + 1));
                        }
                    });
                }
                if next != mode {
                    self.node.lead_set_mode(&id, next);
                }
                ui.separator();
                if mode.listen {
                    let s = self.node.state.read().lead.clone();
                    let (mut agent, mut system) = (s.mix_agent * 100.0, s.mix_system * 100.0);
                    ui.spacing_mut().slider_width = 80.0;
                    ui.label("Працівник");
                    let a = ui.add(egui::Slider::new(&mut agent, 0.0..=150.0).integer().suffix("%")).changed();
                    ui.label("Система");
                    let b = ui.add(egui::Slider::new(&mut system, 0.0..=150.0).integer().suffix("%")).changed();
                    if a || b {
                        self.save_lead(|l| {
                            l.mix_agent = agent / 100.0;
                            l.mix_system = system / 100.0;
                        });
                    }
                    if now_ms() - last_audio < 1500 {
                        ui.colored_label(Color32::from_rgb(108, 203, 95), "● Звук іде");
                    } else {
                        ui.weak("Тиша");
                    }
                }
                let others = activity.iter().filter(|a| a.id != me && (a.watching || a.listening)).count();
                if others > 0 {
                    ui.weak(format!("Ще підключено: {others}"));
                }
                if !online {
                    ui.colored_label(theme::danger(), "Не в мережі");
                } else if on_secure_screen && rights.full_access && has_secure_link {
                    ui.colored_label(Color32::from_rgb(202, 130, 16), "Захищений екран, звук вимкнено");
                } else if locked {
                    ui.colored_label(Color32::from_rgb(202, 130, 16), "Windows заблоковано");
                }
            });
            ui.horizontal_wrapped(|ui| {
                if theme::pick(ui, self.tool.is_none(), format!("{} Вказівник", icon::POINTER)).clicked() {
                    self.tool = None;
                }
                ui.add_enabled_ui(rights.draw, |ui| {
                    for (t, g, label) in [
                        (Tool::Marker, icon::PEN, "Маркер"),
                        (Tool::MarkerSemi, icon::HIGHLIGHT, "Напівпрозорий"),
                        (Tool::Rect, icon::RECT, "Рамка"),
                        (Tool::Arrow, icon::ARROW, "Стрілка"),
                        (Tool::Laser, icon::LASER, "Лазер"),
                    ] {
                        if theme::pick(ui, self.tool == Some(t), format!("{g} {label}")).clicked() {
                            self.tool = Some(t);
                        }
                    }
                    if let Some(tool) = self.tool {
                        let s = self.node.state.read().lead.clone();
                        let (mut value, max) = match tool {
                            Tool::Marker => (s.marker_width, 30.0),
                            Tool::MarkerSemi => (s.semi_width, 60.0),
                            Tool::Rect => (s.rect_width, 20.0),
                            Tool::Arrow => (s.arrow_width, 20.0),
                            Tool::Laser => (s.pointer_size, 30.0),
                        };
                        ui.label("Розмір");
                        if ui.add(egui::Slider::new(&mut value, 1.0..=max).integer()).changed() {
                            self.save_lead(|l| match tool {
                                Tool::Marker => l.marker_width = value,
                                Tool::MarkerSemi => l.semi_width = value,
                                Tool::Rect => l.rect_width = value,
                                Tool::Arrow => l.arrow_width = value,
                                Tool::Laser => l.pointer_size = value,
                            });
                        }
                    }
                    if ui.button(format!("{} Мої", icon::ERASE)).on_hover_text("Стерти мої малюнки").clicked() {
                        self.node.lead_draw(&id, DrawOp::ClearMine);
                    }
                    if ui.button(format!("{} Усі", icon::ERASE)).on_hover_text("Стерти всі малюнки").clicked() {
                        self.node.lead_draw(&id, DrawOp::ClearAll);
                    }
                });
                ui.separator();
                let secure_ok = !on_secure_screen || (rights.full_access && has_secure_link);
                let can_control = rights.control && mode.screen != ScreenLevel::Off && online && secure_ok && (holder.is_empty() || i_control);
                let text = if i_control { "Завершити керування" } else { "Керувати" };
                let hover = if !holder.is_empty() && !i_control {
                    format!("Зараз керує: {}", self.node.member_name(&holder))
                } else {
                    "Потрібен увімкнений екран".to_string()
                };
                if ui.add_enabled(can_control, egui::Button::new(format!("{} {text}", icon::KEYBOARD)).selected(i_control)).on_hover_text(hover).clicked() {
                    self.node.lead_request_control(&id, !i_control);
                    self.tool = None;
                }
                if rights.full_access && has_secure_link && i_control && ui.button("Ctrl+Alt+Del").clicked() {
                    self.node.lead_send_sas(&id);
                }
            });
            ui.horizontal_wrapped(|ui| self.mic_controls(ui));
        });
        egui::TopBottomPanel::bottom("hints").frame(theme::bar_frame()).show(ctx, |ui| {
            ui.horizontal(|ui| self.hint_bar(ui));
        });
        if self.hints_open {
            egui::TopBottomPanel::bottom("hint_history").frame(theme::bar_frame()).show(ctx, |ui| {
                if history.is_empty() {
                    ui.weak("Підказок ще не було");
                }
                egui::ScrollArea::vertical().max_height(150.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                    for h in &history {
                        ui.horizontal(|ui| {
                            ui.weak(format_ts(h.ts));
                            ui.label(&h.text);
                            let (label, color) = match h.state {
                                0 => ("Не прочитано", Color32::from_rgb(202, 130, 16)),
                                2 => ("Прийнято", Color32::from_rgb(108, 203, 95)),
                                3 => ("Відхилено", theme::danger()),
                                4 => ("Проігноровано", Color32::GRAY),
                                _ => ("Прочитано", Color32::GRAY),
                            };
                            ui.colored_label(color, label);
                        });
                    }
                });
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::from_gray(45))).show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
            if mode.screen == ScreenLevel::Off {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, if mode.listen { "Лише звук. Екран вимкнено." } else { "Сеанс на паузі" }, egui::FontId::proportional(16.0), Color32::from_gray(200));
                return;
            }
            if consent != 0 {
                let text = if consent == 1 { "Чекаю на дозвіл власника цього ПК…" } else { "Власник цього ПК відмовив у підключенні" };
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(16.0), Color32::from_gray(200));
                return;
            }
            let Some(tex) = self.textures.get(&id) else {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Очікування зображення…", egui::FontId::proportional(16.0), Color32::from_gray(200));
                return;
            };
            let size = tex.handle.size_vec2();
            let scale = (avail.x / size.x).min(avail.y / size.y);
            let image_rect = Rect::from_center_size(rect.center(), size * scale);
            ui.painter().image(tex.handle.id(), image_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            let scale = image_rect.width() / size.x;
            self.paint_strokes(ui, &id, image_rect, mode.monitor, scale);
            self.paint_preview(ui, image_rect, scale);
            if response.clicked() {
                self.hints_open = false;
            }
            self.update_pointer(ui, &id, image_rect, mode.monitor, i_control);
            if let Some((m, x, y)) = cursor {
                if m == mode.monitor {
                    let p = image_rect.min + Vec2::new(x * image_rect.width(), y * image_rect.height());
                    theme::paint_cursor(ui.painter(), p);
                }
            }
            if i_control {
                self.forward_input(ui, &id, image_rect, &response);
            } else {
                self.handle_drawing(ui, &id, image_rect, &response, mode.monitor);
            }
        });
    }

    /// Line width and opacity of a tool, from this lead's settings.
    fn tool_style(&self, tool: Tool) -> (f32, f32) {
        let s = self.node.state.read().lead.clone();
        match tool {
            Tool::Marker => (s.marker_width, 1.0),
            Tool::MarkerSemi => (s.semi_width, s.semi_alpha),
            Tool::Rect => (s.rect_width, 1.0),
            Tool::Arrow => (s.arrow_width, 1.0),
            Tool::Laser => (0.0, 1.0),
        }
    }

    /// The laser only shows where the lead is pointing, so it follows the cursor without a click.
    fn update_pointer(&mut self, ui: &egui::Ui, id: &str, image: Rect, monitor: u8, controlling: bool) {
        let over = if self.tool == Some(Tool::Laser) && !controlling {
            ui.input(|i| i.pointer.hover_pos()).filter(|p| image.contains(*p))
        } else {
            None
        };
        let size = self.node.state.read().lead.pointer_size;
        match over {
            Some(p) => {
                let at = [(p.x - image.left()) / image.width(), (p.y - image.top()) / image.height()];
                let now = now_ms();
                let elapsed = now - self.pointer_last_ms;
                if (at != self.pointer_last && elapsed >= 33) || elapsed >= 500 {
                    self.node.lead_send_pointer(id, monitor, at[0], at[1], true, size);
                    self.pointer_last = at;
                    self.pointer_last_ms = now;
                    self.pointer_on = true;
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
            None if self.pointer_on => {
                self.node.lead_send_pointer(id, monitor, 0.0, 0.0, false, size);
                self.pointer_on = false;
            }
            None => {}
        }
    }

    fn handle_drawing(&mut self, ui: &egui::Ui, id: &str, image: Rect, r: &egui::Response, monitor: u8) {
        let to_norm = |p: Pos2| ((p.x - image.left()) / image.width(), (p.y - image.top()) / image.height());
        if matches!(self.tool, None | Some(Tool::Laser)) && r.clicked() {
            if let Some(p) = r.interact_pointer_pos().filter(|p| image.contains(*p)) {
                let (x, y) = to_norm(p);
                self.node.lead_send_tap(id, x, y);
            }
        }
        let Some(tool) = self.tool else { return };
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        if tool == Tool::Laser {
            return;
        }
        // A right click during a stroke throws it away.
        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
            if let Some(d) = self.drawing.take() {
                self.node.lead_draw(id, DrawOp::Cancel { id: d });
            }
            self.preview = None;
            return;
        }
        let shape = matches!(tool, Tool::Rect | Tool::Arrow);
        let (width, alpha) = self.tool_style(tool);
        if r.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = r.interact_pointer_pos() {
                let (x, y) = to_norm(p);
                if shape {
                    self.preview = Some((tool, [x, y], [x, y]));
                } else {
                    let draw = self.node.lead_next_draw_id(id);
                    self.drawing = Some(draw);
                    self.node.lead_draw(id, DrawOp::Begin { id: draw, tool, monitor, x, y, width, alpha });
                }
                self.last_move = p;
            }
        }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let (x, y) = to_norm(p);
                if let Some(preview) = self.preview.as_mut() {
                    preview.2 = [x, y];
                } else if let Some(d) = self.drawing {
                    if p.distance(self.last_move) >= 2.0 {
                        self.node.lead_draw(id, DrawOp::Point { id: d, x, y });
                        self.last_move = p;
                    }
                }
            }
        }
        if r.drag_stopped() {
            if let Some(d) = self.drawing.take() {
                self.node.lead_draw(id, DrawOp::End { id: d });
            }
            if let Some((tool, a, b)) = self.preview.take() {
                // Only now does the employee see the finished frame or arrow.
                let draw = self.node.lead_next_draw_id(id);
                self.node.lead_draw(id, DrawOp::Begin { id: draw, tool, monitor, x: a[0], y: a[1], width, alpha });
                self.node.lead_draw(id, DrawOp::Point { id: draw, x: b[0], y: b[1] });
                self.node.lead_draw(id, DrawOp::End { id: draw });
            }
        }
    }

    fn paint_preview(&self, ui: &egui::Ui, image: Rect, scale: f32) {
        let Some((tool, a, b)) = self.preview else { return };
        let map = |p: [f32; 2]| Pos2::new(image.left() + p[0] * image.width(), image.top() + p[1] * image.height());
        let [r, g, bl] = color_for(&self.node.identity.id);
        let stroke = Stroke::new((self.tool_style(tool).0 * scale).max(1.0), Color32::from_rgb(r, g, bl));
        let painter = ui.painter_at(image);
        match tool {
            Tool::Rect => {
                painter.rect_stroke(Rect::from_two_pos(map(a), map(b)), 0.0, stroke);
            }
            _ => {
                painter.arrow(map(a), map(b) - map(a), stroke);
            }
        }
    }

    fn forward_input(&mut self, ui: &egui::Ui, id: &str, image: Rect, r: &egui::Response) {
        let events = ui.input(|i| i.events.clone());
        let hovered = r.hovered() || r.dragged();
        let mods_now = ui.input(|i| i.modifiers);
        for (was, now, vk) in [(self.modifiers.ctrl, mods_now.ctrl, 0x11u16), (self.modifiers.shift, mods_now.shift, 0x10), (self.modifiers.alt, mods_now.alt, 0x12)] {
            if was != now {
                self.node.lead_input(id, InputEv::Key { vk, down: now });
            }
        }
        self.modifiers = mods_now;
        for e in events {
            match e {
                egui::Event::PointerMoved(p) if image.contains(p) && hovered => {
                    let x = (p.x - image.left()) / image.width();
                    let y = (p.y - image.top()) / image.height();
                    self.node.lead_input(id, InputEv::Move { x, y });
                }
                egui::Event::PointerButton { pos, button, pressed, .. } if image.contains(pos) => {
                    if pressed && button == egui::PointerButton::Primary {
                        self.node.lead_send_tap(id, (pos.x - image.left()) / image.width(), (pos.y - image.top()) / image.height());
                    }
                    let b = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Secondary => 1,
                        _ => 2,
                    };
                    self.node.lead_input(id, InputEv::Button { button: b, down: pressed });
                }
                egui::Event::MouseWheel { delta, .. } if hovered => {
                    self.node.lead_input(id, InputEv::Wheel { dy: delta.y.signum() });
                }
                egui::Event::Text(t) => {
                    for c in t.chars() {
                        self.node.lead_input(id, InputEv::Text { ch: c as u32 });
                    }
                }
                egui::Event::Key { key, pressed, modifiers, .. } => {
                    if let Some(vk) = theme::virtual_key(key) {
                        let is_text = (0x30..=0x5A).contains(&vk) || vk == 0x20;
                        if !is_text || modifiers.ctrl || modifiers.alt {
                            self.node.lead_input(id, InputEv::Key { vk, down: pressed });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn paint_strokes(&self, ui: &egui::Ui, id: &str, image: Rect, monitor: u8, scale: f32) {
        let now = now_ms();
        let fade = self.node.state.read().directory.policy.stroke_fade_sec as i64 * 1000;
        let strokes = {
            let l = self.node.lead.lock();
            l.targets.get(id).map(|t| t.strokes.clone()).unwrap_or_default()
        };
        if strokes.is_empty() {
            return;
        }
        let map = |p: [f32; 2]| Pos2::new(image.left() + p[0] * image.width(), image.top() + p[1] * image.height());
        let painter = ui.painter_at(image);
        for s in strokes.iter().filter(|s| s.monitor == monitor && !stroke_expired(s, now, fade)) {
            let alpha = match s.ended_ms {
                Some(e) if now - e > fade => 1.0 - ((now - e - fade) as f32 / lead_core::FADE_MS as f32),
                _ => 1.0,
            };
            let [r, g, b] = s.color;
            let semi = s.tool == Tool::MarkerSemi;
            let opacity = if s.alpha > 0.0 { s.alpha } else if semi { 0.38 } else { 1.0 };
            let c = Color32::from_rgba_unmultiplied(r, g, b, (opacity * alpha.clamp(0.0, 1.0) * 255.0) as u8);
            let width: f32 = ((if s.width > 0.0 { s.width } else if semi { 20.0 } else { 4.0 }) * scale).max(1.0);
            match s.tool {
                Tool::Marker | Tool::MarkerSemi => {
                    painter.add(egui::Shape::line(s.points.iter().map(|p| map(*p)).collect(), Stroke::new(width as f32, c)));
                }
                Tool::Rect if s.points.len() == 2 => {
                    painter.rect_stroke(Rect::from_two_pos(map(s.points[0]), map(s.points[1])), 0.0, Stroke::new(width, c));
                }
                Tool::Arrow if s.points.len() == 2 => {
                    let (a, b2) = (map(s.points[0]), map(s.points[1]));
                    painter.arrow(a, b2 - a, Stroke::new(width, c));
                }
                _ => {}
            }
        }
    }

    fn journal(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(theme::page_frame(ctx)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Журнал");
                ui.add(egui::TextEdit::singleline(&mut self.journal_filter).desired_width(260.0).hint_text("Пошук"));
            });
            ui.separator();
            let needle = self.journal_filter.to_lowercase();
            let log = self.node.audit.lock().clone();
            let rows: Vec<&model::LogEntry> = log
                .iter()
                .rev()
                .filter(|e| needle.is_empty() || format!("{} {} {}", e.lead, e.target, e.action).to_lowercase().contains(&needle))
                .collect();
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, 20.0, rows.len(), |ui, range| {
                for e in &rows[range] {
                    ui.horizontal(|ui| {
                        for (width, text) in [(150.0, format_ts(e.ts)), (160.0, e.lead.clone()), (160.0, e.target.clone())] {
                            ui.allocate_ui_with_layout(Vec2::new(width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_width(width);
                                ui.add(egui::Label::new(text).truncate());
                            });
                        }
                        ui.label(&e.action);
                    });
                }
            });
        });
    }

    fn settings(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("settings_nav").exact_width(240.0).resizable(false).show_separator_line(false).frame(theme::rail_frame()).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.heading("Налаштування");
            });
            ui.add_space(10.0);
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                for (t, glyph, label) in [
                    (SettingsTab::General, icon::SETTINGS, "Загальні"),
                    (SettingsTab::Sound, icon::MIC, "Звук"),
                    (SettingsTab::Drawing, icon::PEN, "Малювання"),
                    (SettingsTab::Hints, icon::SEND, "Підказки"),
                    (SettingsTab::Team, icon::PEOPLE, "Команда"),
                ] {
                    if theme::nav_item(ui, glyph, label, self.tab == t, true).clicked() {
                        self.tab = t;
                        self.draft.message.clear();
                    }
                }
            });
        });
        egui::CentralPanel::default().frame(theme::page_frame(ctx)).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.set_max_width(720.0);
                match self.tab {
                    SettingsTab::General => {
                        self.settings_look(ui);
                        ui.add_space(10.0);
                        self.settings_security(ui);
                        ui.add_space(10.0);
                        self.settings_updates(ui);
                    }
                    SettingsTab::Sound => self.settings_sound(ui),
                    SettingsTab::Drawing => self.settings_drawing(ui),
                    SettingsTab::Hints => {
                        self.settings_help(ui);
                        ui.add_space(10.0);
                        self.settings_phrases(ui);
                    }
                    SettingsTab::Team => {
                        self.settings_devices(ui);
                        ui.add_space(10.0);
                        self.settings_groups(ui);
                        ui.add_space(10.0);
                        self.settings_leads(ui);
                    }
                }
            });
            if !self.draft.message.is_empty() {
                ui.separator();
                ui.label(&self.draft.message);
            }
        });
    }

    fn settings_look(&mut self, ui: &mut egui::Ui) {
        let mut dark = !self.node.state.read().lead.light_theme;
        if theme::toggle(ui, &mut dark, "Темна тема").changed() {
            self.save_lead(|l| l.light_theme = !dark);
            theme::set_mode(ui.ctx(), !dark);
        }
    }

    fn settings_security(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Блокування консолі");
        let mut s = self.node.state.read().lead.clone();
        let before = s.clone();
        theme::toggle(ui, &mut s.lock_enabled, "Блокувати при бездіяльності");
        ui.horizontal(|ui| {
            ui.label("Через, хв:");
            ui.add(egui::DragValue::new(&mut s.lock_idle_min).range(1..=480));
        });
        theme::toggle(ui, &mut s.lock_with_windows, "Блокувати разом із Windows");
        theme::toggle(ui, &mut s.lock_on_sleep, "Блокувати після сну");
        if s != before {
            self.save_lead(|l| *l = s);
        }
        if !self.has_lock_password() {
            ui.colored_label(theme::danger(), "Пароль не задано, блокування не спрацює.");
        }
        ui.add_space(12.0);
        theme::section(ui, "Пароль консолі");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft.new_console_pw).password(true).desired_width(220.0));
            if ui.button("Змінити").clicked() && !self.draft.new_console_pw.is_empty() {
                let h = secure::hash_password(&self.draft.new_console_pw);
                self.draft.new_console_pw.clear();
                self.save_lead(|l| l.console_password = h);
                self.draft.message = "Пароль консолі змінено.".into();
            }
        });
        ui.add_space(12.0);
        theme::section(ui, "Адмінський пароль команди");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft.new_admin_pw).password(true).desired_width(220.0));
            if ui.button("Змінити").clicked() && !self.draft.new_admin_pw.is_empty() {
                let h = secure::hash_password(&self.draft.new_admin_pw);
                self.draft.new_admin_pw.clear();
                self.node.update_directory(|d, me| {
                    d.policy.admin_password = h;
                    d.policy.rev = Rev::new(me);
                });
                self.node.audit_log("—", "Змінено адмінський пароль");
                self.draft.message = "Адмінський пароль змінено.".into();
            }
        });
        ui.add_space(12.0);
        egui::CollapsingHeader::new("Додатково").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft.exit_pw).password(true).desired_width(220.0).hint_text("Адмінський пароль"));
            if ui.button("Вийти з програми").clicked() {
                let admin = self.node.state.read().directory.policy.admin_password.clone();
                if !self.draft.exit_pw.is_empty() && secure::check_password(&admin, &self.draft.exit_pw) {
                    self.node.audit_log("—", "Вихід із програми");
                    std::process::exit(crate::service::EXIT_STOPPED as i32);
                }
                self.draft.exit_pw.clear();
                self.draft.message = "Невірний адмінський пароль.".into();
            }
        });
        ui.add_space(8.0);
        if self.draft.confirm_employee_mode {
            ui.label("Лишиться тільки значок у треї. Повернутись: значок → Налаштування → адмінський пароль.");
            ui.horizontal(|ui| {
                if ui.button("Так, перейти").clicked() {
                    self.node.switch_to_employee_mode();
                }
                if ui.button("Скасувати").clicked() {
                    self.draft.confirm_employee_mode = false;
                }
            });
        } else if ui.button("Перейти в режим працівника").clicked() {
            self.draft.confirm_employee_mode = true;
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft.reset_pw).password(true).desired_width(220.0).hint_text("Адмінський пароль"));
            if ui.button("Скинути до заводських").on_hover_text("Видаляє команду, список ПК, журнал і налаштування на цьому ПК").clicked() {
                let admin = self.node.state.read().directory.policy.admin_password.clone();
                if !self.draft.reset_pw.is_empty() && secure::check_password(&admin, &self.draft.reset_pw) {
                    self.node.factory_reset();
                }
                self.draft.reset_pw.clear();
                self.draft.message = "Невірний адмінський пароль.".into();
            }
        });
        });
    }

    fn settings_sound(&mut self, ui: &mut egui::Ui) {
        if self.draft.devices.is_none() {
            self.draft.devices = Some(crate::audio::list_devices());
        }
        let (mics, speakers) = self.draft.devices.clone().unwrap_or_default();
        let mut s = self.node.state.read().lead.clone();
        let (mic_before, speaker_before) = (s.mic_device.clone(), s.speaker_device.clone());
        let name_of = |list: &[(String, String)], id: &str| list.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_else(|| "Як у Windows".to_string());
        egui::Grid::new("audio").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Мікрофон");
            egui::ComboBox::from_id_salt("mic").width(360.0).selected_text(name_of(&mics, &s.mic_device)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.mic_device, String::new(), "Як у Windows");
                for (id, name) in &mics {
                    ui.selectable_value(&mut s.mic_device, id.clone(), name);
                }
            });
            ui.end_row();
            ui.label("Динаміки");
            egui::ComboBox::from_id_salt("speaker").width(360.0).selected_text(name_of(&speakers, &s.speaker_device)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.speaker_device, String::new(), "Як у Windows");
                for (id, name) in &speakers {
                    ui.selectable_value(&mut s.speaker_device, id.clone(), name);
                }
            });
            ui.end_row();
        });
        if s.mic_device != mic_before || s.speaker_device != speaker_before {
            crate::audio::set_devices(&s.mic_device, &s.speaker_device);
            self.node.state.write().lead = s.clone();
            self.node.save();
            self.draft.message = "Застосується з наступного сеансу.".into();
        }
        if ui.small_button("Оновити список").clicked() {
            self.draft.devices = None;
        }
        ui.add_space(10.0);
        self.settings_mic(ui);
    }

    fn settings_mic(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Гарячі клавіші мікрофона");
        let mut s = self.node.state.read().lead.clone();
        let before = s.clone();
        let key_combo = |ui: &mut egui::Ui, id: &str, label: &str, value: &mut u32| {
            ui.horizontal(|ui| {
                ui.label(label);
                egui::ComboBox::from_id_salt(id).selected_text(format!("F{}", value.saturating_sub(0x6F))).show_ui(ui, |ui| {
                    for n in 1..=12u32 {
                        ui.selectable_value(value, 0x6F + n, format!("F{n}"));
                    }
                });
            });
        };
        key_combo(ui, "ptt", "Утримувати, щоб говорити:", &mut s.ptt_key);
        key_combo(ui, "toggle", "Увімкнути або вимкнути:", &mut s.mic_toggle_key);
        if s != before {
            self.save_lead(|l| *l = s);
        }
    }

    fn update_policy(&self, f: impl FnOnce(&mut model::Policy)) {
        self.node.update_directory(|d, me| {
            f(&mut d.policy);
            d.policy.rev = Rev::new(me);
        });
    }

    fn settings_drawing(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Малювання");
        let mut p = self.node.state.read().directory.policy.clone();
        let before = p.clone();
        let mut s = self.node.state.read().lead.clone();
        let lead_before = s.clone();
        egui::Grid::new("tools").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            for (label, value, max) in [
                ("Маркер", &mut s.marker_width, 30.0),
                ("Напівпрозорий", &mut s.semi_width, 60.0),
                ("Рамка", &mut s.rect_width, 20.0),
                ("Стрілка", &mut s.arrow_width, 20.0),
                ("Лазер", &mut s.pointer_size, 30.0),
            ] {
                ui.label(label);
                ui.add(egui::Slider::new(value, 1.0..=max).integer());
                ui.end_row();
            }
            ui.label("Прозорість");
            ui.add(egui::Slider::new(&mut s.semi_alpha, 0.1..=0.9));
            ui.end_row();
            ui.label("Зникає через, с");
            ui.add(egui::Slider::new(&mut p.stroke_fade_sec, 1..=120));
            ui.end_row();
        });
        if s != lead_before {
            self.save_lead(|l| *l = s);
        }
        ui.add_space(8.0);
        theme::toggle(ui, &mut p.allow_hide_key, "Працівник може сховати малюнки (Ctrl+Alt+H)");
        ui.add_enabled_ui(p.allow_hide_key, |ui| {
            ui.horizontal(|ui| {
                ui.label("Показувати знову через, хв:");
                ui.add(egui::DragValue::new(&mut p.hide_return_min).range(1..=480));
            });
        });
        if p != before {
            self.update_policy(|x| *x = p);
        }
    }

    fn settings_help(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Запити допомоги");
        let mut p = self.node.state.read().directory.policy.clone();
        let before = p.clone();
        theme::toggle(ui, &mut p.show_help_button, "Кнопка допомоги у працівника");
        theme::toggle(ui, &mut p.sound_on_connect, "Звук при підключенні");
        ui.horizontal(|ui| {
            ui.label("Показувати підказку, с");
            ui.add(egui::Slider::new(&mut p.hint_show_sec, 10..=120));
        });
        if p != before {
            self.update_policy(|x| *x = p);
        }
        ui.add_space(8.0);
        ui.label("Причини (кожна з нового рядка):");
        ui.add(egui::TextEdit::multiline(&mut self.draft.reasons).desired_rows(6));
        if ui.button("Зберегти причини").clicked() {
            let list: Vec<String> = self.draft.reasons.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            self.update_policy(|x| x.help_reasons = list);
            self.draft.message = "Збережено.".into();
        }
    }

    fn settings_phrases(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Мої фрази");
        ui.add(egui::TextEdit::multiline(&mut self.draft.personal).desired_rows(4));
        if ui.button("Зберегти мої фрази").clicked() {
            let list: Vec<String> = self.draft.personal.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            self.save_lead(|l| l.personal_phrases = list);
            self.draft.message = "Збережено.".into();
        }
        ui.add_space(12.0);
        theme::section(ui, "Спільні набори команди");
        let sets: Vec<PhraseSet> = self.node.state.read().directory.phrases.values().filter(|p| !p.deleted).cloned().collect();
        for set in sets {
            ui.group(|ui| {
                ui.strong(&set.name);
                let text = self.draft.phrase_edit.entry(set.id.clone()).or_insert_with(|| set.phrases.join("\n"));
                ui.add(egui::TextEdit::multiline(text).desired_rows(4));
                let current = text.clone();
                ui.horizontal(|ui| {
                    if ui.button("Зберегти").clicked() {
                        let list: Vec<String> = current.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                        let id = set.id.clone();
                        self.node.update_directory(|d, me| {
                            if let Some(x) = d.phrases.get_mut(&id) {
                                x.phrases = list;
                                x.rev = Rev::new(me);
                            }
                        });
                        self.draft.message = "Збережено.".into();
                    }
                    if ui.button("Видалити набір").clicked() {
                        let id = set.id.clone();
                        self.node.update_directory(|d, me| {
                            if let Some(x) = d.phrases.get_mut(&id) {
                                x.deleted = true;
                                x.rev = Rev::new(me);
                            }
                        });
                    }
                });
            });
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft.new_set).desired_width(200.0).hint_text("Назва набору"));
            if ui.button("Додати набір").clicked() && !self.draft.new_set.trim().is_empty() {
                let name = std::mem::take(&mut self.draft.new_set);
                self.node.update_directory(|d, me| {
                    let id = secure::b64(&secure::random_bytes::<6>());
                    d.phrases.insert(id.clone(), PhraseSet { id, name, phrases: vec![], deleted: false, rev: Rev::new(me) });
                });
            }
        });
    }

    fn settings_groups(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Групи");
        let groups: Vec<Group> = self.node.state.read().directory.groups.values().filter(|g| !g.deleted).cloned().collect();
        for g in groups {
            ui.horizontal(|ui| {
                ui.label(&g.name);
                if ui.small_button("Видалити").clicked() {
                    let id = g.id.clone();
                    self.node.update_directory(|d, me| {
                        if let Some(x) = d.groups.get_mut(&id) {
                            x.deleted = true;
                            x.rev = Rev::new(me);
                        }
                    });
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft.new_group).desired_width(200.0).hint_text("Назва групи"));
            if ui.button("Додати групу").clicked() && !self.draft.new_group.trim().is_empty() {
                let name = std::mem::take(&mut self.draft.new_group);
                self.node.update_directory(|d, me| {
                    let id = secure::b64(&secure::random_bytes::<6>());
                    d.groups.insert(id.clone(), Group { id, name, deleted: false, rev: Rev::new(me) });
                });
            }
        });
    }

    fn group_options(&self) -> Vec<(String, String)> {
        let st = self.node.state.read();
        let mut v: Vec<(String, String)> = vec![(String::new(), "Без групи".to_string())];
        v.extend(st.directory.groups.values().filter(|g| !g.deleted).map(|g| (g.id.clone(), g.name.clone())));
        v
    }

    fn settings_devices(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Додати ПК за кодом");
        theme::toggle(ui, &mut self.draft.join_role_lead, "Це ПК керівника");
        let opts = self.group_options();
        let current = opts.iter().find(|o| o.0 == self.draft.join_group).map(|o| o.1.clone()).unwrap_or_default();
        egui::ComboBox::from_label("Група").selected_text(current).show_ui(ui, |ui| {
            for (id, name) in &opts {
                ui.selectable_value(&mut self.draft.join_group, id.clone(), name);
            }
        });
        if ui.button("Створити код").clicked() {
            let role = if self.draft.join_role_lead { Role::Lead } else { Role::Agent };
            self.draft.join_code = Some(self.node.lead_create_join_code(role, self.draft.join_group.clone()));
        }
        if let Some(code) = &self.draft.join_code {
            ui.add_space(8.0);
            ui.label(RichText::new(code).size(26.0).strong());
        }
    }

    fn settings_leads(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Права керівників");
        let my_id = self.node.identity.id.clone();
        if !self.node.state.read().directory.member(&my_id).map(|m| m.rights.admin).unwrap_or(false) {
            ui.label("Змінювати права може лише керівник із правом «Адміністрування».");
            return;
        }
        let leads: Vec<model::Member> = self.node.state.read().directory.members.values().filter(|m| m.role == Role::Lead && m.state == MemberState::Approved).cloned().collect();
        let groups = self.group_options();
        for lead in leads {
            ui.group(|ui| {
                ui.strong(&lead.name);
                let mut m = lead.clone();
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut m.rights.view, "Екран");
                    ui.checkbox(&mut m.rights.listen, "Слухати");
                    ui.checkbox(&mut m.rights.draw, "Малювати");
                    ui.checkbox(&mut m.rights.whisper, "Говорити працівнику");
                    ui.checkbox(&mut m.rights.control, "Керування");
                    ui.checkbox(&mut m.rights.admin, "Адміністрування");
                    ui.checkbox(&mut m.rights.full_access, "Повний доступ").on_hover_text("Екран входу, блокування, вікна адміністратора");
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Групи:").on_hover_text("Нічого не вибрано: усі групи");
                    for (id, name) in groups.iter().filter(|g| !g.0.is_empty()) {
                        let mut on = m.lead_groups.contains(id);
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                m.lead_groups.push(id.clone());
                            } else {
                                m.lead_groups.retain(|x| x != id);
                            }
                        }
                    }
                });
                if m != lead {
                    let name = m.name.clone();
                    self.node.update_directory(|d, me| {
                        m.rev = Rev::new(me);
                        d.members.insert(m.id.clone(), m);
                    });
                    self.node.audit_log(&name, "Змінено права керівника");
                }
            });
        }
    }

    fn settings_updates(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Оновлення");
        ui.label(format!("Поточна версія: {}", crate::util::APP_VERSION));
        ui.horizontal(|ui| {
            ui.label("Репозиторій GitHub (власник/назва):");
            ui.add(egui::TextEdit::singleline(&mut self.draft.repo).desired_width(240.0));
            if ui.button("Зберегти").clicked() {
                let repo = self.draft.repo.trim().to_string();
                self.update_policy(|p| p.update_repo = repo);
            }
        });
        ui.weak("В інтернет звертається лише цей ПК і лише після натискання кнопки. Працівники отримують оновлення по локальній мережі.");
        if ui.button("Перевірити й розіслати").clicked() {
            self.draft.update_status = crate::update::check_and_distribute(&self.node);
        }
        ui.label(&self.draft.update_status);
    }

    fn member_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.member_dialog.take() else { return };
        let mut open = true;
        let mut done = false;
        let opts = self.group_options();
        egui::Window::new("Працівник").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.label(format!("Ключ ПК: {}", dlg.id));
            ui.horizontal(|ui| {
                ui.label("Ім'я:");
                ui.add(egui::TextEdit::singleline(&mut dlg.name).desired_width(220.0));
            });
            let current = opts.iter().find(|o| o.0 == dlg.group).map(|o| o.1.clone()).unwrap_or_default();
            egui::ComboBox::from_label("Група").selected_text(current).show_ui(ui, |ui| {
                for (id, name) in &opts {
                    ui.selectable_value(&mut dlg.group, id.clone(), name);
                }
            });
            let mut mute = self.node.state.read().lead.muted_help.contains(&dlg.id);
            if theme::toggle(ui, &mut mute, "Не приймати запити допомоги").changed() {
                let id = dlg.id.clone();
                self.save_lead(|l| {
                    l.muted_help.retain(|x| *x != id);
                    if mute {
                        l.muted_help.push(id);
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Зберегти").clicked() {
                    let (id, name, group) = (dlg.id.clone(), dlg.name.trim().to_string(), dlg.group.clone());
                    let log_name = name.clone();
                    self.node.update_directory(|d, me| {
                        if let Some(x) = d.members.get_mut(&id) {
                            if !name.is_empty() {
                                x.name = name;
                            }
                            x.group = group;
                            x.rev = Rev::new(me);
                        }
                    });
                    self.node.audit_log(&log_name, "Змінено ім'я або групу");
                    done = true;
                }
                if ui.button("Скасувати доступ").clicked() {
                    self.revoke(&dlg.id, &dlg.name);
                    done = true;
                }
            });
        });
        if open && !done {
            self.member_dialog = Some(dlg);
        }
    }
}

impl eframe::App for LeadApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        if !self.opened_maximized {
            // Asked for at window creation this keeps the small size on scaled screens.
            self.opened_maximized = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_to_tray();
        }
        if PASSIVE_RESET.load(Ordering::SeqCst) && crate::agent_ui::console_visible() {
            PASSIVE_RESET.store(false, Ordering::SeqCst);
            self.tabs.clear();
            self.active = None;
            self.textures.clear();
            self.page = Page::Home;
        }
        self.dev_auto_session();
        self.check_auto_lock();
        if self.locked {
            self.lock_screen(ctx);
            return;
        }
        self.nav_rail(ctx);
        self.help_banner(ctx);
        if self.page != Page::Home {
            self.node.lead_set_thumbs(false);
        }
        match self.page {
            Page::Home => self.home(ctx),
            Page::Session => self.session(ctx),
            Page::Journal => self.journal(ctx),
            Page::Settings => self.settings(ctx),
        }
        self.member_dialog(ctx);
        let _ = color_for;
    }
}
