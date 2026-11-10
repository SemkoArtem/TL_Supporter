//! Lead console: employee list or thumbnails, the live session view, journal and settings.

use crate::i18n::tr;
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
    role_pw: String,
    update_busy: bool,
    update_result: std::sync::Arc<parking_lot::Mutex<Option<String>>>,
    old_admin_pw: String,
    owner_pw: String,
    role_error: String,
    devices: Option<(Vec<(String, String)>, Vec<(String, String)>)>,
    add_code: String,
    add_group: String,
    new_group: String,
    new_set: String,
    phrase_edit: HashMap<String, String>,
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
    selected: std::collections::HashSet<String>,
    collapsed: std::collections::HashSet<String>,
    renaming: Option<(String, String)>,
    confirm_delete_group: Option<String>,
    confirm_revoke: bool,
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
        let (reasons, repo, personal) = {
            let st = node.state.read();
            (st.directory.policy.help_reasons.iter().map(|r| tr(r)).collect::<Vec<_>>().join("\n"), st.directory.policy.update_repo.clone(), st.lead.personal_phrases.join("\n"))
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
            hints_open: true,
            selected: Default::default(),
            collapsed: Default::default(),
            renaming: None,
            confirm_delete_group: None,
            confirm_revoke: false,
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
        self.preview = None;
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
                ui.heading(tr("Консоль заблоковано"));
                ui.add_space(12.0);
                let r = ui.add(egui::TextEdit::singleline(&mut self.unlock_input).password(true).desired_width(240.0).hint_text(tr("Пароль")));
                r.request_focus();
                if self.unlock_error {
                    ui.colored_label(theme::danger(), tr("Невірний пароль"));
                }
                ui.add_space(8.0);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button(tr("Розблокувати")).clicked() || enter {
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
        if crate::util::test_env("TLS_DATA_DIR").is_none() || crate::util::test_env("TLS_AUTO_SESSION").is_none() || !self.tabs.is_empty() {
            return;
        }
        if self.last_tick.elapsed().is_err() {
            return;
        }
        let online = self.node.peers.lock().keys().next().cloned();
        if let (Some(id), true) = (online, self.started.elapsed().as_secs() >= 6) {
            self.open_session(&id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false }));
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
                if theme::nav_item(ui, icon::PEOPLE, tr("Працівники"), self.page == Page::Home, expanded).clicked() {
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
                        if ui.button(tr("Завершити сеанс")).clicked() {
                            close = Some(id.clone());
                            ui.close_menu();
                        }
                    });
                    if expanded {
                        let spot = Rect::from_center_size(Pos2::new(row.rect.right() - 22.0, row.rect.center().y), Vec2::splat(28.0));
                        if ui.put(spot, egui::Button::new(icon::CLOSE).frame(false)).on_hover_text(tr("Завершити сеанс")).clicked() {
                            close = Some(id.clone());
                        }
                    }
                }
                if let Some(id) = close {
                    self.close_tab(&id);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    let lock_hint = if self.has_lock_password() { tr("Заблокувати") } else { tr("Спочатку задай пароль") };
                    if theme::nav_item(ui, icon::LOCK, lock_hint, false, expanded).clicked() {
                        self.lock_now();
                    }
                    if theme::nav_item(ui, icon::SETTINGS, tr("Налаштування"), self.page == Page::Settings, expanded).clicked() {
                        go = Some(Page::Settings);
                    }
                    if theme::nav_item(ui, icon::HISTORY, tr("Журнал"), self.page == Page::Journal, expanded).clicked() {
                        go = Some(Page::Journal);
                    }
                    let quiet = self.node.state.read().lead.do_not_disturb;
                    if theme::nav_item(ui, icon::QUIET, if quiet { tr("Не турбувати: увімкнено") } else { tr("Не турбувати") }, quiet, expanded).clicked() {
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

    fn phrases(&self) -> Vec<(String, Vec<String>)> {
        let st = self.node.state.read();
        let mut out: Vec<(String, Vec<String>)> =
            st.directory.phrases.values().filter(|p| !p.deleted).map(|p| (p.name.clone(), p.phrases.clone())).collect();
        if !st.lead.personal_phrases.is_empty() {
            out.insert(0, (tr("Мої").to_string(), st.lead.personal_phrases.clone()));
        }
        out
    }

    /// The conversation with this employee, beside the screen.
    fn chat_panel(&mut self, ctx: &egui::Context, target: &str, history: &[lead_core::HintEntry]) {
        egui::SidePanel::right("chat").exact_width(330.0).resizable(false).show_separator_line(false).frame(theme::rail_frame().inner_margin(10.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Чат")).size(17.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::tool_button(ui, icon::CLOSE, false, tr("Сховати чат")).clicked() {
                        self.hints_open = false;
                    }
                });
            });
            egui::TopBottomPanel::bottom("chat_input").frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(0.0, 8.0))).show_separator_line(false).show_inside(ui, |ui| {
                let phrases = self.phrases();
                ui.menu_button(tr("Готові фрази"), |ui| {
                    for (set, list) in phrases {
                        ui.menu_button(tr(&set), |ui| {
                            for p in list {
                                if ui.button(tr(&p)).clicked() {
                                    self.node.lead_send_hint(target, tr(&p).to_string());
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.hint).desired_width(ui.available_width() - 46.0).hint_text(tr("Текст підказки")));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let send = theme::tool_button(ui, icon::SEND, false, tr("Надіслати")).clicked();
                    if (send || enter) && !self.hint.trim().is_empty() {
                        let text = std::mem::take(&mut self.hint);
                        self.node.lead_send_hint(target, text);
                    }
                });
            });
            egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false; 2]).show(ui, |ui| {
                    if history.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| ui.weak(tr("Підказок ще не було")));
                    }
                    for h in history {
                        let (label, color) = match h.state {
                            0 => (tr("Не прочитано"), Color32::from_rgb(202, 130, 16)),
                            2 => (tr("Прийнято"), Color32::from_rgb(108, 203, 95)),
                            3 => (tr("Відхилено"), theme::danger()),
                            4 => (tr("Проігноровано"), Color32::GRAY),
                            _ => (tr("Прочитано"), Color32::GRAY),
                        };
                        theme::bubble(ui, |ui| {
                            ui.add(egui::Label::new(&h.text).wrap());
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                ui.weak(RichText::new(format_ts(h.ts)).size(11.0));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.colored_label(color, RichText::new(label).size(12.0));
                                });
                            });
                        });
                    }
                });
            });
        });
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
                    let reason = if h.reason.is_empty() { String::new() } else { format!(" — {}", tr(&h.reason)) };
                    let missed = h.is_missed(now_ms());
                    if missed {
                        ui.colored_label(theme::danger(), icon::MISSED).on_hover_text(tr("Ніхто не взяв цей запит"));
                        ui.strong(format!("{name}: {}{reason}", tr("пропущений запит")));
                        ui.weak(format_ts(h.ts));
                    } else {
                        ui.strong(format!("{} {name}: {}{reason}", icon::HELP, tr("потрібна допомога")));
                    }
                    if h.taken_by.is_empty() {
                        if ui.button(if missed { tr("Підключитись") } else { tr("Взяти") }).clicked() {
                            self.node.lead_take_help(&h.target, h.id);
                            self.open_session(&h.target, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false }));
                        }
                    } else {
                        ui.label(format!("{}: {}", tr("Узяв(ла)"), h.taken_by));
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
            ui.horizontal(|ui| {
                ui.heading(tr("Працівники"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let key = self.node.identity.id.clone();
                    self.key_view.show_reversed(ui, &key);
                });
            });
            ui.add_space(8.0);
            theme::card(ui, "", |ui| self.add_by_code(ui));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(260.0).hint_text(tr("Пошук за іменем")));
                ui.add_space(12.0);
                ui.add(egui::TextEdit::singleline(&mut self.draft.new_group).desired_width(180.0).hint_text(tr("Назва групи")));
                if ui.button(tr("Додати групу")).clicked() && !self.draft.new_group.trim().is_empty() {
                    let name = std::mem::take(&mut self.draft.new_group).trim().to_string();
                    self.node.update_directory(|d, me| {
                        let id = secure::b64(&secure::random_bytes::<6>());
                        d.groups.insert(id.clone(), Group { id, name, deleted: false, rev: Rev::new(me) });
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::tool_button(ui, icon::GRID, grid_view, tr("Мініатюри")).clicked() {
                        self.save_lead(|l| l.grid_view = true);
                    }
                    if theme::tool_button(ui, icon::LIST, !grid_view, tr("Список")).clicked() {
                        self.save_lead(|l| l.grid_view = false);
                    }
                });
            });
            ui.add_space(8.0);
            let groups = self.visible_groups();
            let known: std::collections::HashSet<&String> = groups.iter().flat_map(|g| g.2.iter()).collect();
            self.selected.retain(|id| known.contains(id));
            if !self.selected.is_empty() {
                self.selection_bar(ui);
            }
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                self.pending_section(ui);
                if groups.iter().all(|g| g.2.is_empty()) {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| ui.weak(tr("Поки немає жодного ПК. Запусти програму на новому ПК і введи код, який вона покаже.")));
                }
                for (gid, gname, members) in &groups {
                    self.group_header(ui, gid, gname, members);
                    if self.collapsed.contains(gid) {
                        continue;
                    }
                    ui.add_space(4.0);
                    if grid_view {
                        self.grid(ui, ctx, members);
                    } else {
                        for m in members {
                            self.row(ui, m);
                        }
                    }
                    ui.add_space(8.0);
                }
            });
        });
    }

    fn group_header(&mut self, ui: &mut egui::Ui, gid: &str, name: &str, members: &[String]) {
        let online = members.iter().filter(|m| self.node.peer(m).is_some()).count();
        ui.horizontal(|ui| {
            let collapsed = self.collapsed.contains(gid);
            if ui.add(egui::Button::new(if collapsed { icon::CHEVRON_RIGHT } else { icon::CHEVRON_DOWN }).frame(false)).clicked() {
                if collapsed {
                    self.collapsed.remove(gid);
                } else {
                    self.collapsed.insert(gid.to_string());
                }
            }
            let mut all = !members.is_empty() && members.iter().all(|m| self.selected.contains(m));
            if ui.add_enabled(!members.is_empty(), egui::Checkbox::without_text(&mut all)).on_hover_text(tr("Вибрати всю групу")).changed() {
                for m in members {
                    if all {
                        self.selected.insert(m.clone());
                    } else {
                        self.selected.remove(m);
                    }
                }
            }
            let renaming = self.renaming.as_ref().is_some_and(|r| r.0 == gid);
            if renaming {
                let mut text = self.renaming.as_ref().map(|r| r.1.clone()).unwrap_or_default();
                let field = theme::field(ui, &mut text, "", false, 220.0);
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if let Some(r) = self.renaming.as_mut() {
                    r.1 = text.clone();
                }
                if ui.button(tr("Зберегти")).clicked() || enter {
                    let (id, new_name) = (gid.to_string(), text.trim().to_string());
                    if !new_name.is_empty() {
                        self.node.update_directory(|d, me| {
                            if let Some(g) = d.groups.get_mut(&id) {
                                g.name = new_name;
                                g.rev = Rev::new(me);
                            }
                        });
                    }
                    self.renaming = None;
                }
                if ui.button(tr("Скасувати")).clicked() {
                    self.renaming = None;
                }
                return;
            }
            ui.label(RichText::new(name).strong().size(15.0));
            ui.weak(format!("{online}/{}", members.len()));
            if gid.is_empty() {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.confirm_delete_group.as_deref() == Some(gid) {
                    if ui.button(tr("Скасувати")).clicked() {
                        self.confirm_delete_group = None;
                    }
                    if ui.button(tr("Так, видалити")).clicked() {
                        self.delete_group(gid);
                        self.confirm_delete_group = None;
                    }
                    ui.label(tr("ПК перейдуть у «Без групи»"));
                } else {
                    if theme::tool_button(ui, icon::CLOSE, false, tr("Видалити групу")).clicked() {
                        self.confirm_delete_group = Some(gid.to_string());
                    }
                    if theme::tool_button(ui, icon::PEN, false, tr("Перейменувати групу")).clicked() {
                        self.renaming = Some((gid.to_string(), name.to_string()));
                    }
                }
            });
        });
    }

    fn delete_group(&self, gid: &str) {
        let id = gid.to_string();
        self.node.update_directory(|d, me| {
            if let Some(g) = d.groups.get_mut(&id) {
                g.deleted = true;
                g.rev = Rev::new(me);
            }
            for m in d.members.values_mut() {
                let mut changed = false;
                if m.group == id {
                    m.group.clear();
                    changed = true;
                }
                if m.lead_groups.contains(&id) {
                    m.lead_groups.retain(|g| *g != id);
                    changed = true;
                }
                if changed {
                    m.rev = Rev::new(me);
                }
            }
        });
    }

    /// Appears as soon as at least one PC is ticked.
    fn selection_bar(&mut self, ui: &mut egui::Ui) {
        let ids: Vec<String> = self.selected.iter().cloned().collect();
        let opts = self.group_options();
        theme::card(ui, "", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!("{}: {}", tr("Вибрано"), ids.len()));
                ui.add_space(8.0);
                ui.menu_button(tr("Перемістити в групу"), |ui| {
                    for (group, name) in &opts {
                        if ui.button(name).clicked() {
                            let (targets, group) = (ids.clone(), group.clone());
                            self.node.update_directory(|d, me| {
                                for id in &targets {
                                    if let Some(m) = d.members.get_mut(id) {
                                        m.group = group.clone();
                                        m.rev = Rev::new(me);
                                    }
                                }
                            });
                            self.node.audit_log(name, "Переміщено в групу");
                            self.selected.clear();
                            ui.close_menu();
                        }
                    }
                });
                if ids.len() == 1 && ui.button(tr("Перейменувати")).clicked() {
                    self.member_dialog = Some(self.dialog_for(&ids[0]));
                }
                let muted = self.node.state.read().lead.muted_help.clone();
                let all_muted = ids.iter().all(|i| muted.contains(i));
                if ui.button(if all_muted { tr("Увімкнути запити") } else { tr("Вимкнути запити") }).clicked() {
                    let targets = ids.clone();
                    self.save_lead(|l| {
                        l.muted_help.retain(|x| !targets.contains(x));
                        if !all_muted {
                            l.muted_help.extend(targets.iter().cloned());
                        }
                    });
                }
                if self.confirm_revoke {
                    ui.colored_label(theme::danger(), tr("Скасувати доступ вибраним ПК?"));
                    if ui.button(tr("Так, скасувати")).clicked() {
                        for id in &ids {
                            let name = self.node.member_name(id);
                            self.revoke(id, &name);
                        }
                        self.selected.clear();
                        self.confirm_revoke = false;
                    }
                    if ui.button(tr("Ні")).clicked() {
                        self.confirm_revoke = false;
                    }
                } else if ui.button(tr("Скасувати доступ")).clicked() {
                    self.confirm_revoke = true;
                }
                if ui.button(tr("Зняти вибір")).clicked() {
                    self.selected.clear();
                    self.confirm_revoke = false;
                }
            });
        });
    }

    fn visible_groups(&self) -> Vec<(String, String, Vec<String>)> {
        let st = self.node.state.read();
        let needle = self.search.to_lowercase();
        let d = &st.directory;
        let me = &self.node.identity.id;
        let exists = |g: &str| d.groups.get(g).is_some_and(|x| !x.deleted);
        let mut map: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for m in d.members.values() {
            if m.state != MemberState::Approved || &m.id == me || !d.lead_can_reach(me, &m.id) {
                continue;
            }
            if !needle.is_empty() && !m.name.to_lowercase().contains(&needle) {
                continue;
            }
            // A PC whose group is gone belongs with the PCs that have no group.
            let key = if exists(&m.group) { m.group.clone() } else { String::new() };
            map.entry(key).or_default().push(m.id.clone());
        }
        if needle.is_empty() {
            let allowed = d.member(me).map(|m| m.lead_groups.clone()).unwrap_or_default();
            for g in d.groups.values().filter(|g| !g.deleted && (allowed.is_empty() || allowed.contains(&g.id))) {
                map.entry(g.id.clone()).or_default();
            }
        }
        let mut out: Vec<(String, String, Vec<String>)> = map
            .into_iter()
            .map(|(g, mut v)| {
                v.sort_by_key(|id| d.member(id).map(|m| m.name.to_lowercase()).unwrap_or_default());
                let name = tr(&d.group_name(&g)).to_string();
                (g, name, v)
            })
            .collect();
        out.sort_by(|a, b| (a.0.is_empty(), &a.1).cmp(&(b.0.is_empty(), &b.1)));
        out
    }

    /// Add a new PC by the code it shows: find it first, then choose its group.
    fn add_by_code(&mut self, ui: &mut egui::Ui) {
        let check = self.node.lead_check_view();
        ui.horizontal(|ui| {
            ui.label(tr("Додати ПК за кодом:"));
            let field = ui.add(egui::TextEdit::singleline(&mut self.draft.add_code).desired_width(170.0).hint_text(tr("Код із нового ПК")));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add(theme::accent_button(tr("Перевірити"))).clicked() || enter) && !self.draft.add_code.trim().is_empty() {
                self.node.lead_check_code(&self.draft.add_code);
                self.draft.add_group = self.group_options().get(1).map(|g| g.0.clone()).unwrap_or_default();
            }
        });
        let Some(c) = check else { return };
        let opts = self.group_options();
        let group_name = |id: &str| opts.iter().find(|o| o.0 == id).map(|o| o.1.clone()).unwrap_or_default();
        if c.added {
            ui.horizontal(|ui| {
                ui.label(format!("{}: {} «{}».", c.found_name, tr("додано в групу"), group_name(c.group.as_deref().unwrap_or(""))));
                if ui.button(tr("Готово")).clicked() {
                    self.node.lead_check_cancel();
                    self.draft.add_code.clear();
                }
            });
        } else if c.group.is_some() {
            ui.weak(format!("{} {}…", tr("Додаю"), c.found_name));
        } else if !c.found_id.is_empty() {
            ui.horizontal(|ui| {
                ui.label(tr("Знайдено:"));
                ui.strong(&c.found_name);
                ui.weak(&c.found_id);
                let current = group_name(&self.draft.add_group);
                egui::ComboBox::from_id_salt("add_group").selected_text(current).show_ui(ui, |ui| {
                    for (id, name) in &opts {
                        ui.selectable_value(&mut self.draft.add_group, id.clone(), name);
                    }
                });
                if ui.add(theme::accent_button(tr("Додати в групу"))).clicked() {
                    self.node.lead_add_checked(self.draft.add_group.clone());
                }
                if ui.button(tr("Скасувати")).clicked() {
                    self.node.lead_check_cancel();
                }
            });
        } else if c.expires_ms < now_ms() {
            ui.horizontal(|ui| {
                ui.colored_label(theme::danger(), tr("ПК з таким кодом не знайдено. Перевір код і чи відкрито на тому ПК початкове вікно програми."));
                if ui.small_button(icon::CLOSE).clicked() {
                    self.node.lead_check_cancel();
                }
            });
        } else {
            ui.weak(tr("Шукаю ПК з цим кодом…"));
        }
    }

    fn pending_section(&mut self, ui: &mut egui::Ui) {
        let pending: Vec<model::Member> = self.node.state.read().directory.members.values().filter(|m| m.state == MemberState::Pending).cloned().collect();
        if pending.is_empty() {
            return;
        }
        ui.strong(tr("Очікують підтвердження"));
        for m in pending {
            ui.horizontal(|ui| {
                ui.label(format!("{}   {}", m.name, m.id));
                if ui.button(tr("Підтвердити")).clicked() {
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
                if ui.button(tr("Відхилити")).clicked() {
                    self.revoke(&m.id, &m.name);
                }
            });
        }
        ui.separator();
    }

    fn i_am_admin(&self) -> bool {
        self.node.state.read().directory.member(&self.node.identity.id).is_some_and(|m| m.rights.admin)
    }

    /// Without the administration right a lead manages only employees' PCs in their own groups.
    fn can_revoke(&self, id: &str) -> bool {
        let admin = self.i_am_admin();
        let st = self.node.state.read();
        let d = &st.directory;
        let me = &self.node.identity.id;
        let Some(m) = d.member(id) else { return false };
        if d.owner_id == id {
            return false;
        }
        admin || m.state != MemberState::Approved || (m.role == Role::Agent && d.lead_can_reach(me, id))
    }

    fn revoke(&self, id: &str, name: &str) {
        if !self.can_revoke(id) {
            return;
        }
        let target = id.to_string();
        self.node.update_directory(|d, me| {
            if let Some(x) = d.members.get_mut(&target) {
                x.state = MemberState::Revoked;
                x.rev = Rev::new(me);
            }
        });
        self.node.audit_log(name, "Доступ ПК скасовано");
    }

    fn member_status(&self, id: &str, locked: bool) -> (Color32, &'static str) {
        let online = self.node.is_reachable(id);
        if !online {
            (Color32::from_gray(130), tr("Не в мережі"))
        } else if self.node.peer(id).is_none() {
            (Color32::from_rgb(202, 130, 16), tr("Екран входу"))
        } else if locked {
            (Color32::from_rgb(202, 130, 16), tr("Windows заблоковано"))
        } else {
            (Color32::from_rgb(16, 170, 80), tr("У мережі"))
        }
    }

    fn member_flags(&self, ui: &mut egui::Ui, id: &str) {
        if self.node.lead.lock().help.iter().any(|h| h.target == id && h.is_missed(now_ms())) {
            ui.colored_label(theme::danger(), icon::MISSED).on_hover_text(tr("Пропущений запит допомоги"));
        }
        if self.node.state.read().lead.muted_help.iter().any(|m| m == id) {
            ui.weak(icon::QUIET).on_hover_text(tr("Запити допомоги з цього ПК вимкнено"));
        }
    }

    fn row(&mut self, ui: &mut egui::Ui, id: &str) {
        let name = self.node.member_name(id);
        let locked = self.node.lead.lock().targets.get(id).map(|t| t.locked).unwrap_or(false);
        let (dot, status) = self.member_status(id, locked);
        let online = self.node.is_reachable(id);
        theme::row_card(ui, |ui| {
            let mut ticked = self.selected.contains(id);
            if ui.add(egui::Checkbox::without_text(&mut ticked)).changed() {
                if ticked {
                    self.selected.insert(id.to_string());
                } else {
                    self.selected.remove(id);
                }
            }
            let avatar = theme::avatar(ui, &name, color_for(id), dot, 36.0);
            avatar.context_menu(|ui| self.member_menu(ui, id, &name));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(egui::Label::new(RichText::new(&name).strong()).truncate());
                ui.weak(status);
            });
            self.member_flags(ui, id);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::tool_button(ui, icon::MORE, false, tr("Ім'я, група, запити")).clicked() {
                    self.member_dialog = Some(self.dialog_for(id));
                }
                ui.add_enabled_ui(online, |ui| {
                    if theme::tool_button(ui, icon::HEADPHONE, false, tr("Лише слухати")).clicked() {
                        self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Off, monitor: 0, quality: 0, parked: false }));
                    }
                    if ui.add(theme::accent_button(tr("Підключитись"))).clicked() {
                        self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false }));
                    }
                });
            });
        });
    }

    fn dialog_for(&self, id: &str) -> MemberDialog {
        let st = self.node.state.read();
        let m = st.directory.member(id).cloned().unwrap_or_default();
        MemberDialog { id: id.to_string(), name: m.name, group: m.group }
    }

    fn member_menu(&mut self, ui: &mut egui::Ui, id: &str, _name: &str) {
        if ui.button(tr("Перейменувати або перемістити…")).clicked() {
            self.member_dialog = Some(self.dialog_for(id));
            ui.close_menu();
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, members: &[String]) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
            for id in members {
                self.update_thumb(ctx, id);
                let name = self.node.member_name(id);
                let online = self.node.is_reachable(id);
                let locked = self.node.lead.lock().targets.get(id).map(|t| t.locked).unwrap_or(false);
                let (dot, status) = self.member_status(id, locked);
                theme::tile(ui, |ui| {
                    ui.set_width(264.0);
                    let size = Vec2::new(264.0, 148.0);
                    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
                    ui.painter().rect_filled(rect, 4.0, Color32::from_gray(24));
                    if let Some((_, tex)) = self.thumbs.get(id) {
                        let t = tex.size_vec2();
                        let scale = (size.x / t.x).min(size.y / t.y);
                        let img = Rect::from_center_size(rect.center(), t * scale);
                        ui.painter().image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    } else {
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, status, egui::FontId::proportional(13.0), Color32::from_gray(150));
                    }
                    if r.hovered() && online {
                        ui.painter().rect_stroke(rect, 4.0, Stroke::new(2.0_f32, theme::ACCENT));
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if r.clicked() && online {
                        self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false }));
                    }
                    r.context_menu(|ui| self.member_menu(ui, id, &name));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let mut ticked = self.selected.contains(id);
                        if ui.add(egui::Checkbox::without_text(&mut ticked)).changed() {
                            if ticked {
                                self.selected.insert(id.clone());
                            } else {
                                self.selected.remove(id);
                            }
                        }
                        theme::avatar(ui, &name, color_for(id), dot, 26.0);
                        ui.add(egui::Label::new(RichText::new(&name).strong()).truncate());
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(online, |ui| {
                            if ui.add(theme::accent_button(tr("Підключитись"))).clicked() {
                                self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Full, monitor: 0, quality: 0, parked: false }));
                            }
                            if theme::tool_button(ui, icon::HEADPHONE, false, tr("Лише слухати")).clicked() {
                                self.open_session(id, Some(Mode { listen: true, screen: ScreenLevel::Off, monitor: 0, quality: 0, parked: false }));
                            }
                        });
                        if theme::tool_button(ui, icon::MORE, false, tr("Ім'я, група, запити")).clicked() {
                            self.member_dialog = Some(self.dialog_for(id));
                        }
                        self.member_flags(ui, id);
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
        // The tiles are taken out so that the network and sound are not kept waiting for them.
        let tiles = std::mem::take(&mut t.screen.tiles);
        drop(l);
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
        for ((x, y), tile) in tiles {
            if x as usize + tile.w > w || y as usize + tile.h > h {
                continue;
            }
            // A screen has no transparency, so there is nothing to convert.
            let image = egui::ColorImage::from_rgba_premultiplied([tile.w, tile.h], &tile.rgba);
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

        egui::TopBottomPanel::top("session_tools").frame(theme::command_frame()).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                if theme::tool_button(ui, icon::CLOSE, false, tr("Завершити сеанс")).clicked() {
                    self.close_tab(&id);
                }
                theme::divider(ui);
                ui.add_enabled_ui(rights.draw, |ui| {
                    for (t, g, label) in [
                        (Tool::Marker, icon::PEN, tr("Маркер")),
                        (Tool::MarkerSemi, icon::HIGHLIGHT, tr("Напівпрозорий маркер")),
                        (Tool::Rect, icon::RECT, tr("Рамка")),
                        (Tool::Arrow, icon::ARROW, tr("Стрілка")),
                        (Tool::Laser, icon::LASER, tr("Лазер: показує, куди ти вказуєш")),
                    ] {
                        let on = self.tool == Some(t);
                        // A second press turns the tool off and returns the plain cursor.
                        if theme::tool_button(ui, g, on, label).clicked() {
                            self.tool = if on { None } else { Some(t) };
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
                        ui.spacing_mut().slider_width = 90.0;
                        if ui.add(egui::Slider::new(&mut value, 1.0..=max).integer()).on_hover_text(tr("Розмір")).changed() {
                            self.save_lead(|l| match tool {
                                Tool::Marker => l.marker_width = value,
                                Tool::MarkerSemi => l.semi_width = value,
                                Tool::Rect => l.rect_width = value,
                                Tool::Arrow => l.arrow_width = value,
                                Tool::Laser => l.pointer_size = value,
                            });
                        }
                    }
                    theme::flat_menu(ui, icon::ERASE, tr("Стерти мої малюнки"), |ui| {
                        if ui.button(tr("Стерти мої малюнки")).clicked() {
                            self.node.lead_draw(&id, DrawOp::ClearMine);
                            ui.close_menu();
                        }
                        if ui.button(tr("Стерти всі малюнки")).clicked() {
                            self.node.lead_draw(&id, DrawOp::ClearAll);
                            ui.close_menu();
                        }
                    });
                });
                theme::divider(ui);
                let secure_ok = !on_secure_screen || (rights.full_access && has_secure_link);
                let can_control = rights.control && mode.screen != ScreenLevel::Off && online && secure_ok && (holder.is_empty() || i_control);
                let hover = if !holder.is_empty() && !i_control { format!("Зараз керує: {}", self.node.member_name(&holder)) } else { tr("Керування мишею і клавіатурою").to_string() };
                ui.add_enabled_ui(can_control, |ui| {
                    if theme::tool_button(ui, icon::KEYBOARD, i_control, &hover).clicked() {
                        self.node.lead_request_control(&id, !i_control);
                        self.tool = None;
                    }
                });
                if rights.full_access && has_secure_link && i_control && ui.button("Ctrl+Alt+Del").clicked() {
                    self.node.lead_send_sas(&id);
                }
                theme::divider(ui);
                let latched = MIC_LATCHED.load(Ordering::Relaxed);
                let open = MIC_OPEN.load(Ordering::Relaxed);
                if theme::tool_button(ui, if latched { icon::MIC } else { icon::MIC_OFF }, latched, tr("Мікрофон")).clicked() {
                    MIC_LATCHED.fetch_xor(true, Ordering::Relaxed);
                }
                let hold = theme::tool_button(ui, icon::MIC, false, tr("Утримуй, щоб говорити"));
                MIC_BUTTON.store(hold.is_pointer_button_down_on(), Ordering::Relaxed);
                ui.add(egui::ProgressBar::new(crate::audio::mic_level().min(1.0)).desired_width(54.0).rounding(0.0));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
                theme::flat_menu(ui, icon::MORE, tr("Гучність мого мікрофона"), |ui| {
                    let mut gain = self.node.state.read().lead.mic_gain * 100.0;
                    ui.label(tr("Гучність мого мікрофона"));
                    if ui.add(egui::Slider::new(&mut gain, 20.0..=200.0).integer().suffix("%")).changed() {
                        let value = gain / 100.0;
                        crate::audio::set_mic_gain(value);
                        self.save_lead(|l| l.mic_gain = value);
                    }
                    ui.add_space(4.0);
                    ui.add_enabled(false, egui::Button::new(tr("І клієнт"))).on_disabled_hover_text(tr("Потрібен драйвер віртуального мікрофона. Буде додано пізніше."));
                });
                if open {
                    ui.colored_label(theme::danger(), "●");
                }
                if crate::audio::mic_failed() {
                    ui.colored_label(theme::danger(), icon::MISSED).on_hover_text(tr("Мікрофон не відкрився. Перевір пристрій у налаштуваннях"));
                } else if latched && now_ms() - crate::audio::mic_last_sound() > 3000 {
                    ui.colored_label(Color32::from_rgb(202, 130, 16), icon::MISSED).on_hover_text(tr("Немає сигналу: мікрофон вимкнений у Windows або вибрано не той пристрій"));
                }
                theme::divider(ui);
                let mut next = mode;
                if theme::tool_button(ui, icon::HEADPHONE, mode.listen, tr("Слухати")).clicked() {
                    next.listen = !mode.listen;
                }
                let live = mode.listen && now_ms() - last_audio < 1500;
                theme::flat_menu(ui, icon::MORE, tr("Звук системи"), |ui| {
                    let s = self.node.state.read().lead.clone();
                    let (mut agent, mut system) = (s.mix_agent * 100.0, s.mix_system * 100.0);
                    ui.label(tr("Мікрофон працівника"));
                    let a = ui.add(egui::Slider::new(&mut agent, 0.0..=150.0).integer().suffix("%")).changed();
                    ui.label(tr("Звук системи"));
                    let b = ui.add(egui::Slider::new(&mut system, 0.0..=150.0).integer().suffix("%")).changed();
                    if a || b {
                        self.save_lead(|l| {
                            l.mix_agent = agent / 100.0;
                            l.mix_system = system / 100.0;
                        });
                    }
                });
                if live {
                    ui.colored_label(Color32::from_rgb(108, 203, 95), "●").on_hover_text(tr("Звук іде"));
                }
                theme::divider(ui);
                let current = if mode.screen == ScreenLevel::Off { tr("Вимкнено") } else { tr(QUALITIES[mode.quality.min(3) as usize].1) };
                egui::ComboBox::from_id_salt("quality").width(120.0).selected_text(current).show_ui(ui, |ui| {
                    if ui.add(egui::SelectableLabel::new(mode.screen == ScreenLevel::Off, tr("Вимкнено"))).clicked() {
                        next.screen = ScreenLevel::Off;
                    }
                    for (q, label) in QUALITIES {
                        if ui.add(egui::SelectableLabel::new(mode.screen != ScreenLevel::Off && mode.quality == q, tr(label))).clicked() {
                            next.screen = ScreenLevel::Full;
                            next.quality = q;
                        }
                    }
                });
                if monitors.len() > 1 {
                    egui::ComboBox::from_id_salt("mon").width(40.0).selected_text(format!("{}", mode.monitor + 1)).show_ui(ui, |ui| {
                        for m in &monitors {
                            ui.selectable_value(&mut next.monitor, m.index, format!("{}", m.index + 1));
                        }
                    });
                }
                if next != mode {
                    self.node.lead_set_mode(&id, next);
                }
                let others = activity.iter().filter(|a| a.id != me && (a.watching || a.listening)).count();
                if others > 0 {
                    ui.weak(format!("+{others}")).on_hover_text(tr("Ще підключено"));
                }
                if !online {
                    ui.colored_label(theme::danger(), tr("Не в мережі"));
                } else if on_secure_screen && rights.full_access && has_secure_link {
                    ui.colored_label(Color32::from_rgb(202, 130, 16), tr("Захищений екран, звук вимкнено"));
                } else if locked {
                    ui.colored_label(Color32::from_rgb(202, 130, 16), tr("Windows заблоковано"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::tool_button(ui, icon::SEND, self.hints_open, tr("Чат")).clicked() {
                        self.hints_open = !self.hints_open;
                    }
                });
            });
        });
        if self.hints_open {
            self.chat_panel(ctx, &id, &history);
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::from_gray(45))).show(ctx, |ui| {
            let avail = ui.available_size();
            let (rect, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
            if mode.screen == ScreenLevel::Off {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, if mode.listen { tr("Лише звук. Екран вимкнено.") } else { tr("Сеанс на паузі") }, egui::FontId::proportional(16.0), Color32::from_gray(200));
                return;
            }
            if consent != 0 {
                let text = if consent == 1 { tr("Чекаю на дозвіл власника цього ПК…") } else { tr("Власник цього ПК відмовив у підключенні") };
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(16.0), Color32::from_gray(200));
                return;
            }
            let Some(tex) = self.textures.get(&id) else {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tr("Очікування зображення…"), egui::FontId::proportional(16.0), Color32::from_gray(200));
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

    fn press_key(&self, id: &str, vk: u16) {
        self.node.lead_input(id, InputEv::Key { vk, down: true });
        self.node.lead_input(id, InputEv::Key { vk, down: false });
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
                // The interface library turns these shortcuts into events of its own,
                // so the keys have to be pressed on the employee's PC by hand.
                egui::Event::Copy => self.press_key(id, 0x43),
                egui::Event::Cut => self.press_key(id, 0x58),
                egui::Event::Paste(text) => {
                    self.node.lead_send_clipboard(id, text);
                    self.press_key(id, 0x56);
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
                ui.heading(tr("Журнал"));
                ui.add(egui::TextEdit::singleline(&mut self.journal_filter).desired_width(260.0).hint_text(tr("Пошук")));
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
                        ui.label(tr(&e.action));
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
                ui.heading(tr("Налаштування"));
            });
            ui.add_space(10.0);
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                for (t, glyph, label) in [
                    (SettingsTab::General, icon::SETTINGS, tr("Загальні")),
                    (SettingsTab::Sound, icon::MIC, tr("Звук")),
                    (SettingsTab::Drawing, icon::PEN, tr("Малювання")),
                    (SettingsTab::Hints, icon::SEND, tr("Підказки")),
                    (SettingsTab::Team, icon::PEOPLE, tr("Команда")),
                ] {
                    if theme::nav_item(ui, glyph, label, self.tab == t, true).clicked() {
                        self.tab = t;
                        self.draft.message.clear();
                    }
                }
            });
        });
        egui::CentralPanel::default().frame(theme::page_frame(ctx)).show(ctx, |ui| {
            if !self.draft.message.is_empty() {
                ui.colored_label(Color32::from_rgb(96, 205, 255), &self.draft.message);
                ui.add_space(6.0);
            }
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.set_max_width(720.0);
                match self.tab {
                    SettingsTab::General => self.settings_general(ui),
                    SettingsTab::Sound => self.settings_sound(ui),
                    SettingsTab::Drawing => self.settings_drawing(ui),
                    SettingsTab::Hints => self.settings_hints(ui),
                    SettingsTab::Team => self.settings_team(ui),
                }
            });
        });
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, tr("Вигляд"), |ui| {
            egui::Grid::new("look").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label(tr("Мова"));
                crate::i18n::language_picker(ui);
                ui.end_row();
                ui.label(tr("Темна тема"));
                let mut dark = !self.node.state.read().lead.light_theme;
                if theme::toggle(ui, &mut dark, "").changed() {
                    self.save_lead(|l| l.light_theme = !dark);
                    theme::set_mode(ui.ctx(), !dark);
                }
                ui.end_row();
            });
        });
        let mut s = self.node.state.read().lead.clone();
        let before = s.clone();
        theme::card(ui, tr("Блокування консолі"), |ui| {
            theme::toggle(ui, &mut s.lock_enabled, tr("При бездіяльності"));
            ui.add_enabled_ui(s.lock_enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("Через, хв"));
                    ui.add(egui::DragValue::new(&mut s.lock_idle_min).range(1..=480));
                });
            });
            theme::toggle(ui, &mut s.lock_with_windows, tr("Разом із Windows"));
            theme::toggle(ui, &mut s.lock_on_sleep, tr("Після сну"));
            if !self.has_lock_password() {
                ui.colored_label(theme::danger(), tr("Пароль не задано, блокування не спрацює."));
            }
        });
        if s != before {
            self.save_lead(|l| *l = s);
        }
        theme::card(ui, tr("Паролі"), |ui| {
            egui::Grid::new("passwords").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr("Пароль програми"));
                theme::field(ui, &mut self.draft.new_console_pw, "", true, 300.0);
                if ui.button(tr("Змінити")).clicked() && !self.draft.new_console_pw.is_empty() {
                    let h = secure::hash_password(&self.draft.new_console_pw);
                    self.draft.new_console_pw.clear();
                    self.save_lead(|l| l.console_password = h);
                    self.draft.message = tr("Пароль програми змінено.").into();
                }
                ui.end_row();
                let (can_change, current) = {
                    let st = self.node.state.read();
                    (st.directory.member(&self.node.identity.id).is_some_and(|m| m.rights.admin), st.directory.policy.admin_password.clone())
                };
                if !can_change {
                    ui.label(tr("Адмінський пароль команди"));
                    ui.weak(tr("Змінити може лише керівник із правом «Адміністрування»."));
                    ui.end_row();
                    return;
                }
                ui.label(tr("Поточний адмінський пароль"));
                theme::field(ui, &mut self.draft.old_admin_pw, "", true, 300.0);
                ui.end_row();
                ui.label(tr("Новий адмінський пароль"));
                theme::field(ui, &mut self.draft.new_admin_pw, "", true, 300.0);
                if ui.button(tr("Змінити")).clicked() && !self.draft.new_admin_pw.is_empty() {
                    if !secure::check_password(&current, &self.draft.old_admin_pw) {
                        self.draft.message = tr("Невірний адмінський пароль.").into();
                    } else if self.draft.new_admin_pw.chars().count() < 8 {
                        self.draft.message = tr("Пароль має бути не коротшим за 8 символів.").into();
                    } else {
                        let h = secure::hash_password(&self.draft.new_admin_pw);
                        self.draft.new_admin_pw.clear();
                        self.node.update_directory(|d, me| {
                            d.policy.admin_password = h;
                            d.policy.rev = Rev::new(me);
                        });
                        self.node.audit_log("—", "Змінено адмінський пароль");
                        self.draft.message = tr("Адмінський пароль змінено.").into();
                    }
                    self.draft.old_admin_pw.clear();
                }
                ui.end_row();
            });
        });
        theme::card(ui, tr("Оновлення"), |ui| self.settings_updates(ui));
        egui::CollapsingHeader::new(tr("Додатково")).show_unindented(ui, |ui| {
            theme::card(ui, "", |ui| {
                egui::Grid::new("danger").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    theme::field(ui, &mut self.draft.exit_pw, tr("Адмінський пароль"), true, 300.0);
                    if ui.button(tr("Вийти з програми")).clicked() {
                        let admin = self.node.state.read().directory.policy.admin_password.clone();
                        if !self.draft.exit_pw.is_empty() && secure::check_password(&admin, &self.draft.exit_pw) {
                            self.node.audit_log("—", "Вихід із програми");
                            self.node.flush_audit();
                            std::process::exit(crate::service::EXIT_STOPPED as i32);
                        }
                        self.draft.exit_pw.clear();
                        self.draft.message = tr("Невірний адмінський пароль.").into();
                    }
                    ui.end_row();
                    theme::field(ui, &mut self.draft.reset_pw, tr("Адмінський пароль"), true, 300.0);
                    if ui.button(tr("Скинути до заводських")).on_hover_text(tr("Видаляє команду, список ПК, журнал і налаштування на цьому ПК")).clicked() {
                        let admin = self.node.state.read().directory.policy.admin_password.clone();
                        if !self.draft.reset_pw.is_empty() && secure::check_password(&admin, &self.draft.reset_pw) {
                            self.node.factory_reset();
                        }
                        self.draft.reset_pw.clear();
                        self.draft.message = tr("Невірний адмінський пароль.").into();
                    }
                    ui.end_row();
                });
                ui.add_space(8.0);
                if self.draft.confirm_employee_mode {
                    ui.label(tr("Лишиться тільки значок у треї. Повернутись: значок → Налаштування → адмінський пароль."));
                    ui.horizontal(|ui| {
                        if ui.button(tr("Так, перейти")).clicked() {
                            self.node.switch_to_employee_mode();
                        }
                        if ui.button(tr("Скасувати")).clicked() {
                            self.draft.confirm_employee_mode = false;
                        }
                    });
                } else if ui.button(tr("Перейти в режим працівника")).clicked() {
                    self.draft.confirm_employee_mode = true;
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
        let name_of = |list: &[(String, String)], id: &str| list.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_else(|| tr("Як у Windows").to_string());
        theme::card(ui, tr("Пристрої"), |ui| {
            egui::Grid::new("audio").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label(tr("Мікрофон"));
                egui::ComboBox::from_id_salt("mic").width(380.0).selected_text(name_of(&mics, &s.mic_device)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.mic_device, String::new(), tr("Як у Windows"));
                    for (id, name) in &mics {
                        ui.selectable_value(&mut s.mic_device, id.clone(), name);
                    }
                });
                ui.end_row();
                ui.label(tr("Динаміки"));
                egui::ComboBox::from_id_salt("speaker").width(380.0).selected_text(name_of(&speakers, &s.speaker_device)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.speaker_device, String::new(), tr("Як у Windows"));
                    for (id, name) in &speakers {
                        ui.selectable_value(&mut s.speaker_device, id.clone(), name);
                    }
                });
                ui.end_row();
            });
            if ui.small_button(tr("Оновити список")).clicked() {
                self.draft.devices = None;
            }
        });
        if s.mic_device != mic_before || s.speaker_device != speaker_before {
            crate::audio::set_devices(&s.mic_device, &s.speaker_device);
            self.node.state.write().lead = s.clone();
            self.node.save();
            self.draft.message = tr("Застосується з наступного сеансу.").into();
        }
        let mut k = self.node.state.read().lead.clone();
        let before = k.clone();
        let key_combo = |ui: &mut egui::Ui, id: &str, label: &str, value: &mut u32| {
            ui.label(label);
            egui::ComboBox::from_id_salt(id).width(90.0).selected_text(format!("F{}", value.saturating_sub(0x6F))).show_ui(ui, |ui| {
                for n in 1..=12u32 {
                    ui.selectable_value(value, 0x6F + n, format!("F{n}"));
                }
            });
            ui.end_row();
        };
        theme::card(ui, tr("Клавіші мікрофона"), |ui| {
            egui::Grid::new("keys").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                key_combo(ui, "ptt", tr("Утримувати, щоб говорити"), &mut k.ptt_key);
                key_combo(ui, "toggle", tr("Увімкнути або вимкнути"), &mut k.mic_toggle_key);
            });
        });
        if k != before {
            self.save_lead(|l| *l = k);
        }
    }

    fn update_policy(&self, f: impl FnOnce(&mut model::Policy)) {
        if !self.i_am_admin() {
            return;
        }
        self.node.update_directory(|d, me| {
            f(&mut d.policy);
            d.policy.rev = Rev::new(me);
        });
    }

    fn settings_drawing(&mut self, ui: &mut egui::Ui) {
        let admin = self.i_am_admin();
        let mut p = self.node.state.read().directory.policy.clone();
        let before = p.clone();
        let mut s = self.node.state.read().lead.clone();
        let lead_before = s.clone();
        theme::card(ui, tr("Інструменти"), |ui| {
            egui::Grid::new("tools").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                for (label, value, max) in [
                    (tr("Маркер"), &mut s.marker_width, 30.0),
                    (tr("Напівпрозорий"), &mut s.semi_width, 60.0),
                    (tr("Рамка"), &mut s.rect_width, 20.0),
                    (tr("Стрілка"), &mut s.arrow_width, 20.0),
                    (tr("Лазер"), &mut s.pointer_size, 30.0),
                ] {
                    ui.label(label);
                    ui.add(egui::Slider::new(value, 1.0..=max).integer());
                    ui.end_row();
                }
                ui.label(tr("Прозорість"));
                ui.add(egui::Slider::new(&mut s.semi_alpha, 0.1..=0.9));
                ui.end_row();
                ui.label(tr("Зникає через, с"));
                ui.add_enabled(admin, egui::Slider::new(&mut p.stroke_fade_sec, 1..=120));
                ui.end_row();
            });
        });
        theme::card(ui, tr("У працівника"), |ui| {
            if !admin { ui.disable(); }
            theme::toggle(ui, &mut p.allow_hide_key, tr("Може сховати малюнки (Ctrl+Alt+H)"));
            ui.add_enabled_ui(p.allow_hide_key, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("Повертати через, хв"));
                    ui.add(egui::DragValue::new(&mut p.hide_return_min).range(1..=480));
                });
            });
        });
        if s != lead_before {
            self.save_lead(|l| *l = s);
        }
        if p != before {
            self.update_policy(|x| *x = p);
        }
    }

    fn settings_hints(&mut self, ui: &mut egui::Ui) {
        let admin = self.i_am_admin();
        let mut p = self.node.state.read().directory.policy.clone();
        let before = p.clone();
        theme::card(ui, tr("Запити допомоги"), |ui| {
            if !admin { ui.disable(); }
            theme::toggle(ui, &mut p.show_help_button, tr("Кнопка допомоги у працівника"));
            theme::toggle(ui, &mut p.sound_on_connect, tr("Звук при підключенні"));
            ui.horizontal(|ui| {
                ui.label(tr("Показувати підказку, с"));
                ui.add(egui::Slider::new(&mut p.hint_show_sec, 10..=120));
            });
            ui.add_space(6.0);
            ui.label(tr("Причини, кожна з нового рядка"));
            ui.add(egui::TextEdit::multiline(&mut self.draft.reasons).desired_rows(4).desired_width(f32::INFINITY));
            if ui.button(tr("Зберегти")).clicked() {
                let list: Vec<String> = self.draft.reasons.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                self.update_policy(|x| x.help_reasons = list);
                self.draft.message = tr("Збережено.").into();
            }
        });
        if p != before {
            self.update_policy(|x| *x = p);
        }
        theme::card(ui, tr("Мої фрази"), |ui| {
            ui.add(egui::TextEdit::multiline(&mut self.draft.personal).desired_rows(4).desired_width(f32::INFINITY));
            if ui.button(tr("Зберегти")).clicked() {
                let list: Vec<String> = self.draft.personal.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                self.save_lead(|l| l.personal_phrases = list);
                self.draft.message = tr("Збережено.").into();
            }
        });
        let sets: Vec<PhraseSet> = self.node.state.read().directory.phrases.values().filter(|p| !p.deleted).cloned().collect();
        theme::card(ui, tr("Спільні фрази команди"), |ui| {
            if !admin { ui.disable(); }
            for set in sets {
                ui.strong(tr(&set.name));
                let text = self.draft.phrase_edit.entry(set.id.clone()).or_insert_with(|| set.phrases.iter().map(|p| tr(p)).collect::<Vec<_>>().join("\n"));
                ui.add(egui::TextEdit::multiline(text).desired_rows(3).desired_width(f32::INFINITY));
                let current = text.clone();
                ui.horizontal(|ui| {
                    if ui.button(tr("Зберегти")).clicked() {
                        let list: Vec<String> = current.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                        let id = set.id.clone();
                        self.node.update_directory(|d, me| {
                            if let Some(x) = d.phrases.get_mut(&id) {
                                x.phrases = list;
                                x.rev = Rev::new(me);
                            }
                        });
                        self.draft.message = tr("Збережено.").into();
                    }
                    if ui.button(tr("Видалити набір")).clicked() {
                        let id = set.id.clone();
                        self.node.update_directory(|d, me| {
                            if let Some(x) = d.phrases.get_mut(&id) {
                                x.deleted = true;
                                x.rev = Rev::new(me);
                            }
                        });
                    }
                });
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.draft.new_set).desired_width(220.0).hint_text(tr("Назва набору")));
                if ui.button(tr("Додати набір")).clicked() && !self.draft.new_set.trim().is_empty() {
                    let name = std::mem::take(&mut self.draft.new_set);
                    self.node.update_directory(|d, me| {
                        let id = secure::b64(&secure::random_bytes::<6>());
                        d.phrases.insert(id.clone(), PhraseSet { id, name, phrases: vec![], deleted: false, rev: Rev::new(me) });
                    });
                }
            });
        });
    }

    fn group_options(&self) -> Vec<(String, String)> {
        let st = self.node.state.read();
        let mut v: Vec<(String, String)> = vec![(String::new(), tr("Без групи").to_string())];
        // A lead limited to some groups cannot move PCs into the others.
        let me = st.directory.member(&self.node.identity.id);
        let admin = me.is_some_and(|m| m.rights.admin);
        let allowed = me.map(|m| m.lead_groups.clone()).unwrap_or_default();
        v.extend(st.directory.groups.values().filter(|g| !g.deleted && (admin || allowed.is_empty() || allowed.contains(&g.id))).map(|g| (g.id.clone(), tr(&g.name).to_string())));
        v
    }

    fn settings_team(&mut self, ui: &mut egui::Ui) {
        let (no_owner, admin, hash) = {
            let st = self.node.state.read();
            let admin = st.directory.member(&self.node.identity.id).is_some_and(|m| m.rights.admin);
            (st.directory.owner_id.is_empty(), admin, st.directory.policy.admin_password.clone())
        };
        if no_owner && admin {
            theme::card(ui, tr("Головний ПК"), |ui| {
                ui.label(tr("Головний ПК ще не вибрано. Лише він зможе давати право «Адміністрування», а в інших керівників це право буде знято."));
                ui.horizontal(|ui| {
                    theme::field(ui, &mut self.draft.owner_pw, tr("Адмінський пароль"), true, 240.0);
                    if ui.button(tr("Зробити цей ПК головним")).clicked() {
                        if !self.draft.owner_pw.is_empty() && secure::check_password(&hash, &self.draft.owner_pw) {
                            self.node.claim_main_pc();
                            self.draft.message = tr("Збережено.").into();
                        } else {
                            self.draft.message = tr("Невірний адмінський пароль.").into();
                        }
                        self.draft.owner_pw.clear();
                    }
                });
            });
        }
        theme::card(ui, tr("Права керівників"), |ui| self.settings_leads(ui));
    }

    fn settings_leads(&mut self, ui: &mut egui::Ui) {
        let my_id = self.node.identity.id.clone();
        if !self.node.state.read().directory.member(&my_id).map(|m| m.rights.admin).unwrap_or(false) {
            ui.label(tr("Змінювати права може лише керівник із правом «Адміністрування»."));
            return;
        }
        let leads: Vec<model::Member> = self.node.state.read().directory.members.values().filter(|m| m.role == Role::Lead && m.state == MemberState::Approved).cloned().collect();
        let groups = self.group_options();
        let owner = self.node.state.read().directory.owner_id.clone();
        for lead in leads {
            ui.strong(&lead.name);
            if lead.id == owner && owner != my_id {
                ui.weak(tr("Головний ПК"));
                ui.add_space(8.0);
                continue;
            }
            let mut m = lead.clone();
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut m.rights.view, tr("Екран"));
                ui.checkbox(&mut m.rights.listen, tr("Слухати"));
                ui.checkbox(&mut m.rights.draw, tr("Малювати"));
                ui.checkbox(&mut m.rights.whisper, tr("Говорити працівнику"));
                ui.checkbox(&mut m.rights.control, tr("Керування"));
                ui.add_enabled(owner == my_id && lead.id != my_id, egui::Checkbox::new(&mut m.rights.admin, tr("Адміністрування")))
                    .on_disabled_hover_text(tr("Це право може дати лише головний ПК"));
                ui.checkbox(&mut m.rights.full_access, tr("Повний доступ")).on_hover_text(tr("Екран входу, блокування, вікна адміністратора"));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Групи:")).on_hover_text(tr("Нічого не вибрано: усі групи"));
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
            ui.add_space(8.0);
        }
    }

    fn settings_updates(&mut self, ui: &mut egui::Ui) {
        let admin = self.i_am_admin();
        if let Some(result) = self.draft.update_result.lock().take() {
            self.draft.update_status = result;
            self.draft.update_busy = false;
        }
        ui.label(format!("{}: {}", tr("Поточна версія"), crate::util::APP_VERSION));
        ui.horizontal(|ui| {
            ui.label("GitHub");
            ui.add(egui::TextEdit::singleline(&mut self.draft.repo).desired_width(240.0).hint_text(tr("власник/назва")));
            if ui.add_enabled(admin, egui::Button::new(tr("Зберегти"))).clicked() {
                let repo = self.draft.repo.trim().to_string();
                self.update_policy(|p| p.update_repo = repo);
            }
        });
        if ui.add_enabled(admin && !self.draft.update_busy, egui::Button::new(tr("Перевірити й розіслати"))).clicked() {
            // Downloading can take minutes and the window must keep working meanwhile.
            self.draft.update_busy = true;
            self.draft.update_status = tr("Перевіряю…").to_string();
            let (node, result, repaint) = (self.node.clone(), self.draft.update_result.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                *result.lock() = Some(crate::update::check_and_distribute(&node));
                repaint.request_repaint();
            });
        }
        ui.label(&self.draft.update_status);
    }

    fn member_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.member_dialog.take() else { return };
        let mut open = true;
        let mut done = false;
        let opts = self.group_options();
        egui::Window::new(tr("Працівник")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.label(format!("{}: {}", tr("Ключ ПК"), dlg.id));
            let main_pc = self.node.state.read().directory.owner_id == dlg.id;
            ui.horizontal(|ui| {
                ui.label(tr("Ім'я:"));
                ui.add(egui::TextEdit::singleline(&mut dlg.name).desired_width(220.0));
            });
            let current = opts.iter().find(|o| o.0 == dlg.group).map(|o| o.1.clone()).unwrap_or_default();
            egui::ComboBox::from_label(tr("Група")).selected_text(current).show_ui(ui, |ui| {
                for (id, name) in &opts {
                    ui.selectable_value(&mut dlg.group, id.clone(), name);
                }
            });
            let mut mute = self.node.state.read().lead.muted_help.contains(&dlg.id);
            if theme::toggle(ui, &mut mute, tr("Не приймати запити допомоги")).changed() {
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
                if ui.button(tr("Зберегти")).clicked() {
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
                if ui.add_enabled(self.can_revoke(&dlg.id), egui::Button::new(tr("Скасувати доступ"))).clicked() {
                    self.revoke(&dlg.id, &dlg.name);
                    done = true;
                }
            });
            let (can_manage, role) = {
                let st = self.node.state.read();
                (st.directory.member(&self.node.identity.id).is_some_and(|m| m.rights.admin), st.directory.member(&dlg.id).map(|m| m.role))
            };
            if can_manage && !main_pc {
                ui.add_space(6.0);
                ui.separator();
                let to_lead = role == Some(Role::Agent);
                ui.horizontal(|ui| {
                    theme::field(ui, &mut self.draft.role_pw, tr("Адмінський пароль"), true, 190.0);
                    let label = if to_lead { tr("Зробити керівником") } else { tr("Зробити працівником") };
                    if ui.button(label).on_hover_text(tr("Програма на тому ПК перезапуститься")).clicked() {
                        let admin = self.node.state.read().directory.policy.admin_password.clone();
                        // Leads without a group are hidden from the others, so the PC would be lost from view.
                        let no_place = to_lead && {
                            let st = self.node.state.read();
                            st.directory.member(&dlg.id).is_some_and(|m| m.group.is_empty()) && !st.directory.groups.values().any(|g| !g.deleted)
                        };
                        if no_place {
                            self.draft.role_error = tr("Спершу створи групу для цього ПК.").into();
                        } else if !self.draft.role_pw.is_empty() && secure::check_password(&admin, &self.draft.role_pw) {
                            self.node.set_member_role(&dlg.id, if to_lead { Role::Lead } else { Role::Agent });
                            self.draft.role_error.clear();
                            done = true;
                        } else {
                            self.draft.role_error = tr("Невірний адмінський пароль.").into();
                        }
                        self.draft.role_pw.clear();
                    }
                });
                if !self.draft.role_error.is_empty() {
                    ui.colored_label(theme::danger(), &self.draft.role_error);
                }
            }
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
        // Only the session being looked at receives a picture.
        let visible = if self.page == Page::Session { self.active.clone() } else { None };
        for tab in &self.tabs {
            self.node.lead_set_parked(tab, Some(tab) != visible.as_ref());
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
