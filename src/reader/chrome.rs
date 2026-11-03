//! Reader window chrome. One row does everything: the window's identity on the
//! left, reading controls in the middle, progress + window controls on the
//! right. Its bottom edge doubles as the progress bar. Nothing floats over the
//! page except in fullscreen, where the same row slides in on demand.

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, RichText, Sense, Stroke};

use super::{Action, Env, Reader, Side};
use crate::book::{Content, Loc};
use crate::store::{self, Columns, HlColor, Layout, ReaderSettings, THEMES};
use crate::ui::{self, text_button, ui_b, ui_m, Icon, Palette, ACCENT};

pub(super) const BAR_H: f32 = 52.0;

impl Reader {
    pub fn viewport_id(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("reader", &self.id))
    }

    pub fn viewport_builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(format!("{} — {}", self.book.meta.title, crate::APP_NAME))
            .with_app_id("seshat")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([560.0, 420.0])
            .with_decorations(false)
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(crate::logo::icon_data(128)))
    }

    /// Apply a change to whichever settings this book uses (per-book or global).
    pub(super) fn set(&mut self, env: &mut Env, f: impl FnOnce(&mut ReaderSettings)) {
        if let Some(s) = self.data.settings.as_mut() {
            f(s);
            self.dirty = true;
        } else {
            f(&mut env.app.reader);
        }
    }

    pub(super) fn cur(&self) -> Loc {
        match &self.book.content {
            Content::Flow(_) => self.loc,
            Content::Fixed(_) => Loc::new(self.fx.page, 0, 0),
        }
    }

    pub(super) fn cover_tex(&mut self) -> Option<egui::TextureId> {
        let bytes = self.book.meta.cover.clone()?;
        self.tex.request("__cover", move || Some(crate::texload::Src::Res(crate::book::ResRef::Bytes(std::sync::Arc::new(bytes)))), 900);
        self.tex.get("__cover").map(|t| t.tex.id())
    }

    pub(super) fn title_bar(&mut self, ctx: &egui::Context, s: &ReaderSettings, pal: &Palette, env: &mut Env, actions: &mut Vec<Action>) {
        let desk = ui::desk(pal);
        egui::TopBottomPanel::top("r-title").exact_height(BAR_H).show_separator_line(false).frame(egui::Frame::new().fill(desk).inner_margin(egui::Margin { left: 10, right: 10, top: 0, bottom: 0 })).show(ctx, |ui| {
            let rect = ui.max_rect();
            let drag = ui.interact(rect, ui.id().with("drag"), Sense::click_and_drag());
            ui::titlebar_behaviour(ctx, &drag);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui::icon_button(ui, Icon::Back, "Library (Esc)").clicked() {
                    actions.push(Action::Close);
                }
                let (cr, _) = ui.allocate_exact_size(vec2(22.0, 32.0), Sense::hover());
                match self.cover_tex() {
                    Some(id) => {
                        ui.painter().image(id, cr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => ui::paint_placeholder_cover(ui.painter(), cr, &self.book.meta.title, ""),
                }
                ui.add_space(6.0);
                let title_w = (ui.available_width() - 840.0).clamp(90.0, 260.0);
                ui.allocate_ui(vec2(title_w, BAR_H), |ui| {
                    ui.set_width(title_w);
                    ui.vertical(|ui| {
                        ui.add_space(9.0);
                        ui.add(egui::Label::new(RichText::new(&self.book.meta.title).font(ui_b(14.5))).truncate());
                        let chapter = self.chapter_title(self.cur());
                        let sub = if chapter.is_empty() || chapter == self.book.meta.title { self.book.meta.author.clone() } else { chapter };
                        ui.add(egui::Label::new(RichText::new(sub).font(ui_m(11.5)).color(pal.muted)).truncate());
                    });
                });
                sep(ui, pal);
                self.tools_left(ui, ctx, s, pal, env);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if ui::window_controls(ui, pal.fg) {
                        actions.push(Action::Close);
                    }
                    ui.add_space(6.0);
                    let pct = (self.fraction() * 100.0).round();
                    ui.label(RichText::new(format!("{pct}%")).font(ui_b(14.0)).color(ACCENT));
                    sep(ui, pal);
                    if ui::icon_toggle(ui, Icon::Fullscreen, "Fullscreen (F)", self.fullscreen).clicked() {
                        self.toggle_fullscreen(ctx);
                    }
                    self.tools_right(ui, s, pal, env);
                });
            });
            let f = ctx.animate_value_with_time(ui.id().with("prog"), self.fraction(), 0.35);
            ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, pal.border));
            ui.painter().line_segment([pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.left() + rect.width() * f, rect.bottom() - 1.0)], Stroke::new(3.0, ACCENT));
        });
    }

    /// Navigation, layout, type size, theme.
    fn tools_left(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, s: &ReaderSettings, pal: &Palette, env: &mut Env) {
        let flow = self.book.flow().is_some();
        if ui::icon_toggle(ui, Icon::Menu, "Contents, notes, bookmarks (C)", self.side.is_some()).clicked() {
            self.side = if self.side.is_some() { None } else { Some(Side::Toc) };
            self.toc_scroll = true;
        }
        if ui::icon_toggle(ui, Icon::Search, "Find in book (Ctrl+K)", self.side == Some(Side::Search)).clicked() {
            self.side = Some(Side::Search);
            self.focus_search = true;
        }
        if ui::icon_toggle(ui, Icon::Note, "Notebook (N)", self.notebook).clicked() {
            self.notebook = !self.notebook;
            self.settings_open = false;
        }
        if !self.history.is_empty() && ui::icon_button(ui, Icon::Undo, "Back to where you were (Backspace)").clicked() {
            self.back();
        }
        sep(ui, pal);
        if flow {
            let sel = match (s.layout, s.columns) {
                (Layout::Scrolled, _) => 2,
                (_, Columns::One) => 0,
                _ => 1,
            };
            if let Some(i) = ui::segmented(ui, "layout", &["Page", "Spread", "Scroll"], sel) {
                self.set(env, |s| match i {
                    0 => {
                        s.layout = Layout::Paginated;
                        s.columns = Columns::One;
                    }
                    1 => {
                        s.layout = Layout::Paginated;
                        s.columns = Columns::Two;
                    }
                    _ => s.layout = Layout::Scrolled,
                });
            }
            sep(ui, pal);
            if text_button(ui, "A−", false).on_hover_text("Smaller text (Ctrl −)").clicked() {
                self.set(env, |s| s.font_size = (s.font_size - 1.0).max(10.0));
            }
            ui.label(RichText::new(format!("{:.0}", s.font_size)).font(ui_m(13.0)).color(pal.muted));
            if text_button(ui, "A+", false).on_hover_text("Larger text (Ctrl +)").clicked() {
                self.set(env, |s| s.font_size = (s.font_size + 1.0).min(48.0));
            }
        } else {
            let z = self.fx.zoom as usize;
            if let Some(i) = ui::segmented(ui, "zoom", &["Fit page", "Fit width"], z) {
                self.fx.zoom = i as u8;
                self.fx.scale = 1.0;
            }
            if ui::icon_button(ui, Icon::ZoomOut, "Zoom out").clicked() {
                self.fx.scale = (self.fx.scale / 1.15).max(0.3);
            }
            if ui::icon_button(ui, Icon::ZoomIn, "Zoom in").clicked() {
                self.fx.scale = (self.fx.scale * 1.15).min(5.0);
            }
            if text_button(ui, "RTL", self.fx.rtl).on_hover_text("Right-to-left page order (manga)").clicked() {
                self.fx.rtl = !self.fx.rtl;
            }
        }
        sep(ui, pal);
        // theme: one swatch; every theme in a small popover
        let p = ui::palette_for(s.theme, s.custom_bg, s.custom_fg);
        let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
        let k = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().circle_filled(r.center(), 8.5 + 1.5 * k, p.bg);
        ui.painter().circle_stroke(r.center(), 8.5 + 1.5 * k, Stroke::new(1.5, pal.border.lerp_to_gamma(ACCENT, k)));
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "a", ui_m(10.0), p.fg);
        let pid = ui.make_persistent_id("theme-pop");
        let resp = resp.on_hover_text("Theme");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pid));
        }
        egui::popup_below_widget(ui, pid, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.horizontal(|ui| {
                for t in THEMES {
                    let p = ui::palette_for(*t, s.custom_bg, s.custom_fg);
                    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 46.0), Sense::click());
                    let c = r.center() - vec2(0.0, 7.0);
                    ui.painter().circle_filled(c, 12.0, p.bg);
                    ui.painter().circle_stroke(c, 12.0, Stroke::new(if s.theme == *t { 2.0 } else { 1.0 }, if s.theme == *t { ACCENT } else { pal.border }));
                    ui.painter().text(c, egui::Align2::CENTER_CENTER, "a", ui_m(11.0), p.fg);
                    ui.painter().text(pos2(c.x, r.bottom() - 6.0), egui::Align2::CENTER_CENTER, t.name(), ui_m(9.5), pal.muted);
                    if resp.clicked() {
                        self.set(env, |s| s.theme = *t);
                    }
                }
            });
        });
    }

    /// Reading modes and the rest (laid out right-to-left).
    fn tools_right(&mut self, ui: &mut egui::Ui, s: &ReaderSettings, pal: &Palette, env: &mut Env) {
        let flow = self.book.flow().is_some();
        if text_button(ui, "Aa", self.settings_open).on_hover_text("All reading settings").clicked() {
            self.settings_open = !self.settings_open;
            if self.settings_open {
                self.notebook = false;
            }
        }
        // page-turn motion, right where you'd look for it
        let label = ["Flip: Full", "Flip: Simple", "Flip: Off"][s.motion.min(2) as usize];
        let r = text_button(ui, label, false).on_hover_text("Page-turn motion");
        let pid = ui.make_persistent_id("motion-pop");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pid));
        }
        egui::popup_below_widget(ui, pid, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(230.0);
            for (i, (name, desc)) in [("Full", "Cover opens, pages curl over the spine"), ("Simple", "A quick fade between pages"), ("Off", "Pages change instantly")].iter().enumerate() {
                let on = s.motion as usize == i;
                let resp = ui.add(egui::SelectableLabel::new(on, RichText::new(format!("{name}\n{desc}")).font(ui_m(12.5))));
                if resp.clicked() {
                    self.set(env, |s| s.motion = i as u8);
                }
            }
        });
        sep(ui, pal);
        if text_button(ui, "Auto", self.auto).on_hover_text(if flow && s.layout == Layout::Scrolled { "Auto-scroll (A)" } else { "Turn pages automatically (A)" }).clicked() {
            self.auto = !self.auto;
            self.auto_last = std::time::Instant::now();
        }
        if flow {
            let playing = self.speaker.is_some();
            if ui::icon_toggle(ui, Icon::Speaker, "Read aloud", playing).clicked() {
                if playing {
                    self.stop_tts();
                } else {
                    self.start_tts(self.loc, env.app.tts_rate);
                }
            }
        }
        let bm = self.bookmark_here().is_some();
        if ui::icon_toggle(ui, if bm { Icon::BookmarkOn } else { Icon::Bookmark }, "Bookmark this page (B)", bm).clicked() {
            self.toggle_bookmark();
        }
        if flow {
            if let Some(c) = self.hl_mode {
                for col in store::HL_COLORS.iter().rev() {
                    let [r, g, b] = col.rgb();
                    let (rr, resp) = ui.allocate_exact_size(vec2(12.0, 22.0), Sense::click());
                    let on = *col == c;
                    ui.painter().rect_filled(Rect::from_center_size(rr.center(), vec2(9.0, if on { 16.0 } else { 9.0 })), CornerRadius::same(3), Color32::from_rgb(r, g, b));
                    if resp.clicked() {
                        self.hl_mode = Some(*col);
                    }
                }
            }
            if ui::icon_toggle(ui, Icon::Pen, "Highlighter: selections become highlights (H)", self.hl_mode.is_some()).clicked() {
                self.hl_mode = if self.hl_mode.is_some() { None } else { Some(HlColor::Yellow) };
            }
        }
    }

    /// Fullscreen only: the same row slides in when the pointer nears the top.
    pub(super) fn floating_toolbar(&mut self, ctx: &egui::Context, s: &ReaderSettings, pal: &Palette, env: &mut Env) {
        if !self.fullscreen {
            return;
        }
        let near = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y < 64.0);
        let t = ctx.animate_bool_with_time(egui::Id::new("float-tools"), near || self.tools || self.settings_open, 0.2);
        if t < 0.01 {
            return;
        }
        let w = ctx.screen_rect().width();
        egui::Area::new("float-tools".into()).fixed_pos(pos2(0.0, -BAR_H * (1.0 - super::ease(t)))).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.multiply_opacity(t);
            egui::Frame::new().fill(ui::desk(pal)).inner_margin(egui::Margin::symmetric(12, 0)).show(ui, |ui| {
                ui.set_width(w - 24.0);
                ui.set_height(BAR_H);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    self.tools_left(ui, ctx, s, pal, env);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if text_button(ui, "Exit fullscreen", false).clicked() {
                            self.toggle_fullscreen(ctx);
                        }
                        sep(ui, pal);
                        self.tools_right(ui, s, pal, env);
                    });
                });
            });
            let r = ui.min_rect();
            ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, pal.border));
        });
    }

    pub(super) fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    /// Auto-advance: timed page turns, or a steady scroll.
    pub(super) fn tick_auto(&mut self, ctx: &egui::Context, s: &ReaderSettings) {
        if !self.auto {
            return;
        }
        let dt = self.auto_last.elapsed().as_secs_f32();
        if self.book.flow().is_some() && s.layout == Layout::Scrolled {
            self.auto_last = std::time::Instant::now();
            self.scroll_by(s.auto_px * dt.min(0.1));
            ctx.request_repaint();
        } else {
            if dt >= s.auto_secs {
                self.auto_last = std::time::Instant::now();
                self.next_page();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Go to a percentage (or page, for PDFs/comics).
    pub(super) fn goto_field(&mut self, ui: &mut egui::Ui) {
        let flow = self.book.flow().is_some();
        let r = ui.add(egui::TextEdit::singleline(&mut self.goto_text).id(egui::Id::new("goto")).hint_text(if flow { "go to %" } else { "page" }).desired_width(64.0).margin(vec2(8.0, 4.0)));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Ok(v) = self.goto_text.trim().trim_end_matches('%').parse::<f32>() {
                let target = match &self.book.content {
                    Content::Flow(f) => f.loc_at_fraction(v / 100.0),
                    Content::Fixed(fx) => Loc::new((v.max(1.0) as usize - 1).min(fx.pages - 1), 0, 0),
                };
                self.jump(target, true);
            }
            self.goto_text.clear();
        }
    }
}

fn sep(ui: &mut egui::Ui, pal: &Palette) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, pal.border));
    ui.add_space(4.0);
}
