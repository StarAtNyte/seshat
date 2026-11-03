//! Reader window chrome. One row does everything: the window's identity on the
//! left, reading controls in the middle, progress + window controls on the
//! right. Its bottom edge doubles as the progress bar. Nothing floats over the
//! page except in fullscreen, where the same row slides in on demand.

use crate::i18n::tr;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, RichText, Sense, Stroke};

use super::{Action, Env, Reader, Side};
use crate::book::{Content, Loc};
use crate::store::{self, Columns, HlColor, Layout, ReaderSettings, THEMES};
use crate::ui::{self, text_button, ui_b, ui_m, Icon, Palette, ACCENT};

pub(super) const BAR_H: f32 = 52.0;

impl Reader {
    /// Expose the current page's text to screen readers as a polite live region, so page
    /// changes are announced and the content can be read. Only costs anything when a screen
    /// reader (AccessKit) is active.
    pub(super) fn a11y_page(&mut self, ctx: &egui::Context) {
        // egui has no "is a screen reader attached" query; the live-region write below only
        // succeeds when AccessKit is on, and we remember that for the next frame.
        let on = ctx.data(|d| d.get_temp::<bool>(egui::Id::new("a11y-on"))).unwrap_or(false);
        let (pos, body) = if !on {
            (String::new(), String::new())
        } else {
            self.a11y_text()
        };
        let label = format!("{}. {}. {}", self.book.meta.title, pos, body.trim());
        let id = egui::Id::new("a11y-page");
        egui::Area::new(id).order(egui::Order::Background).fixed_pos(pos2(0.0, 0.0)).interactable(false).show(ctx, |ui| {
            let r = ui.allocate_response(vec2(1.0, 1.0), Sense::focusable_noninteractive());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label.clone()));
            let now = ctx.accesskit_node_builder(r.id, |n| n.set_live(egui::accesskit::Live::Polite)).is_some();
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("a11y-on"), now));
        });
    }

    fn a11y_text(&mut self) -> (String, String) {
        let book = self.book.clone();
        if book.fixed().is_some() {
            self.start_texts();
        }
        let (pos, body) = match &book.content {
            Content::Flow(f) => {
                let mut text = String::new();
                if let Some(sec) = f.sections.get(self.loc.section) {
                    for b in sec.blocks.iter().skip(self.loc.block) {
                        text.push_str(&b.text());
                        text.push('\n');
                        if text.chars().count() > 2400 {
                            break;
                        }
                    }
                }
                (self.position_text(0.0), text)
            }
            Content::Fixed(fx) => {
                let t = self.fx.texts.as_ref().and_then(|t| t.get(self.fx.page).cloned()).unwrap_or_default();
                (trf!("Page {n} of {total}", n = self.fx.page + 1, total = fx.pages), t)
            }
        };
        (pos, body)
    }

    pub fn viewport_id(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("reader", &self.id))
    }

    pub fn viewport_builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(format!("{} — {}", self.book.meta.title, crate::APP_NAME))
            .with_app_id("seshat")
            .with_inner_size([1280.0, 860.0])
            .with_maximized(true)
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
            // three zones: identity on the left, reading tools centred on the
            // window, everything else on the right; widths from the last frame
            let bar = rect;
            let (mid_id, right_id) = (ui.id().with("mid-w"), ui.id().with("right-w"));
            let mid_w: f32 = ctx.data(|d| d.get_temp(mid_id)).unwrap_or(420.0);
            let right_w: f32 = ctx.data(|d| d.get_temp(right_id)).unwrap_or(420.0);
            let gap = 16.0;
            let mid_left = (bar.center().x - mid_w / 2.0).min(bar.right() - right_w - gap - mid_w).max(bar.left() + 180.0);
            let row = |r: Rect, l: egui::Layout| egui::UiBuilder::new().max_rect(r).layout(l);
            let lr = egui::Layout::left_to_right(egui::Align::Center);
            // right zone
            let used = ui.scope_builder(row(bar, egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui::window_controls(ui, pal.fg) {
                    actions.push(Action::Close);
                }
                ui.add_space(6.0);
                let pct = (self.fraction() * 100.0).round();
                ui.label(RichText::new(format!("{pct}%")).font(ui_b(14.0)).color(ACCENT));
                sep(ui, pal);
                if ui::icon_toggle(ui, Icon::Fullscreen, tr("Fullscreen (F)"), self.fullscreen).clicked() {
                    self.toggle_fullscreen(ctx);
                }
                self.tools_right(ui, s, pal, env);
                ui.min_rect().width()
            }).inner;
            // middle zone
            let mid_rect = Rect::from_min_max(pos2(mid_left, bar.top()), pos2((bar.right() - used - gap).max(mid_left + 40.0), bar.bottom()));
            let mid_used = ui.scope_builder(row(mid_rect, lr), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                self.tools_left(ui, ctx, s, pal, env);
                ui.min_rect().width()
            }).inner;
            // left zone: back, panel, cover, title (takes what is left)
            ui.scope_builder(row(Rect::from_min_max(bar.min, pos2(mid_left - gap, bar.bottom())), lr), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui::icon_button(ui, Icon::Back, tr("Library (Esc)")).clicked() {
                    actions.push(Action::Close);
                }
                if ui::icon_toggle(ui, Icon::PanelLeft, tr("Contents, notes, bookmarks (C)"), self.side.is_some()).clicked() {
                    self.side = if self.side.is_some() { None } else { Some(Side::Toc) };
                    self.toc_scroll = true;
                }
                // search opens the sidebar's Find tab, so it lives beside the sidebar toggle
                if ui::icon_toggle(ui, Icon::Search, tr("Find in book (Ctrl+K)"), self.side == Some(Side::Search)).clicked() {
                    self.side = Some(Side::Search);
                    self.focus_search = true;
                }
                if !self.history.is_empty() && ui::icon_button(ui, Icon::Undo, tr("Back to where you were (Backspace)")).clicked() {
                    self.back();
                }
                ui.add_space(8.0);
                let (cr, _) = ui.allocate_exact_size(vec2(22.0, 32.0), Sense::hover());
                match self.cover_tex() {
                    Some(id) => {
                        ui.painter().image(id, cr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => ui::paint_placeholder_cover(ui.painter(), cr, &crate::book::script::visual(&self.book.meta.title), ""),
                }
                ui.add_space(8.0);
                let title_w = ui.available_width().clamp(40.0, 340.0);
                ui.allocate_ui(vec2(title_w, BAR_H), |ui| {
                    ui.set_width(title_w);
                    let chapter = self.chapter_title(self.cur());
                    let sub = if chapter.is_empty() || chapter == self.book.meta.title { self.book.meta.author.clone() } else { chapter };
                    ui.vertical(|ui| {
                        // one line: centred on the bar; two lines: centred as a pair
                        ui.add_space(if sub.trim().is_empty() { (BAR_H - 18.0) / 2.0 } else { 9.0 });
                        ui.add(egui::Label::new(RichText::new(crate::book::script::visual(&self.book.meta.title)).font(ui_b(14.5))).truncate());
                        if !sub.trim().is_empty() {
                            ui.add(egui::Label::new(RichText::new(crate::book::script::visual(&sub)).font(ui_m(11.5)).color(pal.muted)).truncate());
                        }
                    });
                });
            });
            if (mid_used - mid_w).abs() > 0.5 || (used - right_w).abs() > 0.5 {
                ctx.data_mut(|d| {
                    d.insert_temp(mid_id, mid_used);
                    d.insert_temp(right_id, used);
                });
                ctx.request_repaint();
            }
            let f = ctx.animate_value_with_time(ui.id().with("prog"), self.fraction(), 0.35);
            ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, pal.border));
            ui.painter().line_segment([pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.left() + rect.width() * f, rect.bottom() - 1.0)], Stroke::new(3.0, ACCENT));
        });
    }

    /// Navigation, layout, type size, theme.
    fn tools_left(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, s: &ReaderSettings, pal: &Palette, env: &mut Env) {
        let flow = self.book.flow().is_some();
        let w = ctx.screen_rect().width();
        let (compact, tight) = (w < 1500.0 * crate::i18n::width_factor(), w < 1180.0 * crate::i18n::width_factor());
        if flow {
            let sel = match (s.layout, s.columns) {
                (Layout::Scrolled, _) => 2,
                (_, Columns::One) => 0,
                _ => 1,
            };
            let picked = if tight { None } else { ui::segmented(ui, "layout", &[tr("Page"), tr("Spread"), tr("Scroll")], sel) };
            if let Some(i) = picked {
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
            if text_button(ui, "A−", false).on_hover_text(tr("Smaller text (Ctrl −)")).clicked() {
                self.set(env, |s| s.font_size = (s.font_size - 1.0).max(10.0));
            }
            if !compact {
                ui.label(RichText::new(format!("{:.0}", s.font_size)).font(ui_m(13.0)).color(pal.muted));
            }
            if text_button(ui, "A+", false).on_hover_text(tr("Larger text (Ctrl +)")).clicked() {
                self.set(env, |s| s.font_size = (s.font_size + 1.0).min(48.0));
            }
        } else {
            let z = self.fx.zoom as usize;
            if let Some(i) = ui::segmented(ui, "zoom", &[tr("Fit page"), tr("Fit width")], z) {
                self.fx.zoom = i as u8;
                self.fx.scale = 1.0;
            }
            if ui::icon_button(ui, Icon::ZoomOut, tr("Zoom out")).clicked() {
                self.fx.scale = (self.fx.scale / 1.15).max(0.3);
            }
            if ui::icon_button(ui, Icon::ZoomIn, tr("Zoom in")).clicked() {
                self.fx.scale = (self.fx.scale * 1.15).min(5.0);
            }
            if text_button(ui, "RTL", self.fx.rtl).on_hover_text(tr("Right-to-left page order (manga)")).clicked() {
                self.fx.rtl = !self.fx.rtl;
            }
        }
        if tight {
            return;
        }
        sep(ui, pal);
        // theme: one swatch; every theme in a small popover
        let p = ui::palette_for(s.theme, s.custom_bg, s.custom_fg);
        let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
        ui::name(&resp, tr("Theme"), None);
        let k = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().circle_filled(r.center(), 8.5 + 1.5 * k, p.bg);
        ui.painter().circle_stroke(r.center(), 8.5 + 1.5 * k, Stroke::new(1.5, pal.border.lerp_to_gamma(ACCENT, k)));
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "a", ui_m(10.0), p.fg);
        let pid = ui.make_persistent_id("theme-pop");
        let resp = resp.on_hover_text(tr("Theme"));
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pid));
        }
        egui::popup_below_widget(ui, pid, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.horizontal(|ui| {
                for t in THEMES {
                    let p = ui::palette_for(*t, s.custom_bg, s.custom_fg);
                    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 46.0), Sense::click());
                    ui::name(&resp, &format!("{} theme", t.name()), Some(s.theme == *t));
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
        let w = ui.ctx().screen_rect().width();
        let (compact, tight) = (w < 1500.0 * crate::i18n::width_factor(), w < 1180.0 * crate::i18n::width_factor());
        // right-hand panels live on the right
        if ui::icon_toggle(ui, Icon::PanelRight, tr("Notebook (N)"), self.notebook).clicked() {
            self.notebook = !self.notebook;
            self.settings_open = false;
        }
        if text_button(ui, "Aa", self.settings_open).on_hover_text(tr("All reading settings")).clicked() {
            self.settings_open = !self.settings_open;
            if self.settings_open {
                self.notebook = false;
            }
        }
        // page-turn motion, right where you'd look for it
        let label = if compact { tr("Flip") } else { [tr("Flip: Full"), tr("Flip: Simple"), tr("Flip: Off")][s.motion.min(2) as usize] };
        let r = text_button(ui, label, false).on_hover_text(tr("Page-turn motion"));
        let pid = ui.make_persistent_id("motion-pop");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pid));
        }
        egui::popup_below_widget(ui, pid, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(230.0);
            for (i, (name, desc)) in [(tr("Full"), tr("Cover opens, pages curl over the spine")), (tr("Simple"), tr("A quick fade between pages")), (tr("Off"), tr("Pages change instantly"))].iter().enumerate() {
                let on = s.motion as usize == i;
                let resp = ui.add(egui::SelectableLabel::new(on, RichText::new(format!("{name}\n{desc}")).font(ui_m(12.5))));
                if resp.clicked() {
                    self.set(env, |s| s.motion = i as u8);
                }
            }
        });
        sep(ui, pal);
        if !compact && text_button(ui, tr("Auto"), self.auto).on_hover_text(if flow && s.layout == Layout::Scrolled { tr("Auto-scroll (A)") } else { tr("Turn pages automatically (A)") }).clicked() {
            self.auto = !self.auto;
            self.auto_last = std::time::Instant::now();
        }
        if flow && !tight {
            let playing = self.speaker.is_some();
            if ui::icon_toggle(ui, Icon::Speaker, if self.book.meta.overlay.is_some() && self.narration { tr("Play the book's narration") } else { tr("Read aloud") }, playing).clicked() {
                if playing {
                    self.stop_tts();
                } else {
                    self.start_tts(self.loc, env.app);
                }
            }
        }
        let bm = self.bookmark_here().is_some();
        if ui::icon_toggle(ui, if bm { Icon::BookmarkOn } else { Icon::Bookmark }, tr("Bookmark this page (B)"), bm).clicked() {
            self.toggle_bookmark();
        }
        if flow {
            if let Some(c) = self.hl_mode {
                for col in store::HL_COLORS.iter().rev() {
                    let [r, g, b] = col.rgb();
                    let (rr, resp) = ui.allocate_exact_size(vec2(12.0, 22.0), Sense::click());
                    let on = *col == c;
                    ui::name(&resp, &format!("{:?} highlight colour", col), Some(on));
                    ui.painter().rect_filled(Rect::from_center_size(rr.center(), vec2(9.0, if on { 16.0 } else { 9.0 })), CornerRadius::same(3), Color32::from_rgb(r, g, b));
                    if resp.clicked() {
                        self.hl_mode = Some(*col);
                    }
                }
            }
            if ui::icon_toggle(ui, Icon::Pen, tr("Highlighter: selections become highlights (H)"), self.hl_mode.is_some()).clicked() {
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
                    if ui::icon_toggle(ui, Icon::PanelLeft, tr("Contents (C)"), self.side.is_some()).clicked() {
                        self.side = if self.side.is_some() { None } else { Some(Side::Toc) };
                    }
                    self.tools_left(ui, ctx, s, pal, env);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if text_button(ui, tr("Exit fullscreen"), false).clicked() {
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
        let r = ui.add(egui::TextEdit::singleline(&mut self.goto_text).id(egui::Id::new("goto")).hint_text(if flow { tr("go to %") } else { tr("page") }).desired_width(64.0).margin(vec2(8.0, 4.0)));
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
