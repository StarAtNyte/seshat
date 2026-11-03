//! Sidebar (contents / notes / bookmarks / search) and the reading settings panel.

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense, Stroke};

use super::{Action, Env, Reader, Side};
use crate::book::{Content, Loc};
use crate::store::{self, Columns, Layout, ReaderSettings, Theme, THEMES};
use crate::ui::{self, chip, icon_button, Icon, Palette, ACCENT};

pub(super) fn sidebar(r: &mut Reader, ctx: &egui::Context, side: Side, pal: &Palette, env: &mut Env) {
    egui::SidePanel::left("reader-side")
        .resizable(true)
        .default_width(330.0)
        .width_range(240.0..=560.0)
        .frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::same(12)).stroke(Stroke::new(1.0, pal.border)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 40.0);
                    ui.add(egui::Label::new(RichText::new(&r.book.meta.title).strong().size(16.0)).truncate());
                    if !r.book.meta.author.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&r.book.meta.author).color(pal.muted)).truncate());
                    }
                });
                if icon_button(ui, Icon::Close, "Close sidebar").clicked() {
                    r.side = None;
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (s, label) in [(Side::Toc, "Contents"), (Side::Notes, "Notes"), (Side::Bookmarks, "Bookmarks"), (Side::Search, "Search")] {
                    let count = match s {
                        Side::Notes => r.data.annotations.len(),
                        Side::Bookmarks => r.data.bookmarks.len(),
                        _ => 0,
                    };
                    let text = if count > 0 { format!("{label} {count}") } else { label.to_string() };
                    if chip(ui, side == s, &text).clicked() {
                        r.side = Some(s);
                        if s == Side::Search {
                            r.focus_search = true;
                        }
                        if s == Side::Toc {
                            r.toc_scroll = true;
                        }
                    }
                }
            });
            ui.separator();
            match side {
                Side::Toc => toc(r, ui, pal),
                Side::Notes => notes(r, ui, pal),
                Side::Bookmarks => bookmarks(r, ui, pal),
                Side::Search => search(r, ui, pal, env),
            }
        });
}

fn cur_loc(r: &Reader) -> Loc {
    match &r.book.content {
        Content::Flow(_) => r.loc,
        Content::Fixed(_) => Loc::new(r.fx.page, 0, 0),
    }
}

fn toc(r: &mut Reader, ui: &mut egui::Ui, pal: &Palette) {
    let cur = r.book.toc_index_for(cur_loc(r));
    if r.book.toc.is_empty() {
        ui.label(RichText::new("This book has no table of contents.").color(pal.muted));
        return;
    }
    let mut jump = None;
    let scroll_now = std::mem::take(&mut r.toc_scroll);
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let book = r.book.clone();
        for (i, e) in book.toc.iter().enumerate() {
            let active = cur == Some(i);
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
            if active {
                ui.painter().rect_filled(rect, CornerRadius::same(6), ACCENT.linear_multiply(0.14));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(6), pal.hover);
            }
            let indent = 10.0 + e.depth as f32 * 16.0;
            let font = egui::FontId::proportional(if e.depth == 0 { 14.0 } else { 13.5 });
            let color = if active { ACCENT } else if e.depth == 0 { pal.fg } else { pal.muted.lerp_to_gamma(pal.fg, 0.4) };
            let galley = ui.painter().layout(e.title.clone(), font, color, w - indent - 50.0);
            let g = if galley.rows.len() > 1 { ui.painter().layout_no_wrap(truncate(&e.title, ((w - indent - 50.0) / 7.0) as usize), egui::FontId::proportional(13.5), color) } else { galley };
            ui.painter().galley(rect.left_center() + vec2(indent, -g.size().y / 2.0), g, color);
            if let Content::Flow(f) = &book.content {
                let pct = (f.fraction(e.loc) * 100.0).round();
                ui.painter().text(rect.right_center() - vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{pct}%"), egui::FontId::proportional(11.0), pal.muted);
            } else {
                ui.painter().text(rect.right_center() - vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{}", e.loc.section + 1), egui::FontId::proportional(11.0), pal.muted);
            }
            if active && scroll_now {
                resp.scroll_to_me(Some(egui::Align::Center));
            }
            if resp.on_hover_text(&e.title).clicked() {
                jump = Some(e.loc);
            }
        }
    });
    if let Some(l) = jump {
        r.jump(l, true);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn notes(r: &mut Reader, ui: &mut egui::Ui, pal: &Palette) {
    if r.data.annotations.is_empty() {
        ui.label(RichText::new("Select text in the book to highlight it or add a note.").color(pal.muted));
        return;
    }
    ui.horizontal(|ui| {
        if ui.button("Export Markdown…").clicked() {
            export_notes(r);
        }
        if ui.button("Copy all").clicked() {
            ui.ctx().copy_text(notes_markdown(r));
        }
    });
    ui.add_space(4.0);
    let mut jump = None;
    let mut delete = None;
    let mut edit = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let mut last_chapter = String::new();
        for a in &r.data.annotations {
            if a.chapter != last_chapter {
                ui.add_space(6.0);
                ui.label(RichText::new(&a.chapter).strong().size(12.5).color(pal.muted));
                last_chapter = a.chapter.clone();
            }
            let [cr, cg, cb] = a.color.rgb();
            egui::Frame::new().fill(pal.hover).corner_radius(8).inner_margin(egui::Margin { left: 12, right: 8, top: 6, bottom: 6 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let rect = ui.max_rect();
                ui.painter().rect_filled(egui::Rect::from_min_size(rect.left_top() - vec2(8.0, 0.0), vec2(3.0, rect.height())), CornerRadius::same(2), Color32::from_rgb(cr, cg, cb));
                let resp = ui.add(egui::Label::new(RichText::new(truncate(&a.text, 300)).size(13.5)).sense(Sense::click()));
                if !a.note.is_empty() {
                    ui.label(RichText::new(&a.note).italics().size(13.0).color(ACCENT));
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(store::relative_time(a.created)).size(11.0).color(pal.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Delete").clicked() {
                            delete = Some(a.id);
                        }
                        if ui.small_button(if a.note.is_empty() { "Add note" } else { "Edit note" }).clicked() {
                            edit = Some((a.id, a.note.clone()));
                        }
                    });
                });
                if resp.clicked() {
                    jump = Some(a.start);
                }
            });
            ui.add_space(4.0);
        }
    });
    if let Some(l) = jump {
        r.jump(l, true);
    }
    if let Some(id) = delete {
        r.data.annotations.retain(|a| a.id != id);
        r.dirty = true;
    }
    if edit.is_some() {
        r.note_edit = edit;
    }
}

fn notes_markdown(r: &Reader) -> String {
    let mut s = format!("# {}\n", r.book.meta.title);
    if !r.book.meta.author.is_empty() {
        s.push_str(&format!("*{}*\n", r.book.meta.author));
    }
    let mut last = String::new();
    for a in &r.data.annotations {
        if a.chapter != last {
            s.push_str(&format!("\n## {}\n", a.chapter));
            last = a.chapter.clone();
        }
        s.push_str(&format!("\n> {}\n", a.text.replace('\n', "\n> ")));
        if !a.note.is_empty() {
            s.push_str(&format!("\n{}\n", a.note));
        }
    }
    s
}

fn export_notes(r: &Reader) {
    let name = format!("{} - notes.md", r.book.meta.title.replace(['/', '\\', ':'], "-"));
    if let Some(p) = rfd::FileDialog::new().set_file_name(&name).add_filter("Markdown", &["md"]).save_file() {
        let _ = std::fs::write(p, notes_markdown(r));
    }
}

fn bookmarks(r: &mut Reader, ui: &mut egui::Ui, pal: &Palette) {
    if ui.button(if r.bookmark_here().is_some() { "Remove bookmark on this page" } else { "Bookmark this page" }).clicked() {
        r.toggle_bookmark();
    }
    ui.add_space(4.0);
    if r.data.bookmarks.is_empty() {
        ui.label(RichText::new("No bookmarks yet. Press Ctrl+B to bookmark the current page.").color(pal.muted));
        return;
    }
    let mut jump = None;
    let mut del = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (i, b) in r.data.bookmarks.iter().enumerate() {
            egui::Frame::new().fill(pal.hover).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&b.chapter).size(12.0).color(pal.muted));
                if ui.add(egui::Label::new(RichText::new(truncate(&b.excerpt, 140)).size(13.5)).sense(Sense::click())).clicked() {
                    jump = Some(b.loc);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(store::relative_time(b.created)).size(11.0).color(pal.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            del = Some(i);
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }
    });
    if let Some(l) = jump {
        r.jump(l, true);
    }
    if let Some(i) = del {
        r.data.bookmarks.remove(i);
        r.dirty = true;
    }
}

fn search(r: &mut Reader, ui: &mut egui::Ui, pal: &Palette, _env: &mut Env) {
    let resp = ui.add(egui::TextEdit::singleline(&mut r.search_q).hint_text("Search in book…").desired_width(f32::INFINITY));
    if std::mem::take(&mut r.focus_search) {
        resp.request_focus();
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        r.run_search();
        resp.request_focus();
    }
    ui.horizontal(|ui| {
        if r.search_running {
            ui.spinner();
            ui.label(RichText::new(format!("{} results so far…", r.search_hits.len())).color(pal.muted));
        } else if !r.search_hits.is_empty() {
            ui.label(RichText::new(format!("{} results", r.search_hits.len())).color(pal.muted));
            if ui.small_button("◀").on_hover_text("Previous result").clicked() {
                let i = r.search_cur.map(|i| i.saturating_sub(1)).unwrap_or(0);
                r.search_cur = Some(i);
                let l = r.search_hits[i].loc;
                r.jump(l, false);
                r.search_cur = Some(i);
            }
            if ui.small_button("▶").on_hover_text("Next result").clicked() {
                let i = r.search_cur.map(|i| (i + 1).min(r.search_hits.len() - 1)).unwrap_or(0);
                let l = r.search_hits[i].loc;
                r.jump(l, false);
                r.search_cur = Some(i);
            }
        } else if r.search_q.trim().chars().count() >= 2 && r.search_gen > 0 {
            ui.label(RichText::new("No results").color(pal.muted));
        }
    });
    if r.book.fixed().is_some() && r.fx.texts.is_none() && r.search_running {
        ui.label(RichText::new("Extracting PDF text (first search only)…").size(12.0).color(pal.muted));
    }
    ui.separator();
    let mut jump = None;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, 58.0, r.search_hits.len(), |ui, range| {
        for i in range {
            let h = &r.search_hits[i];
            let active = r.search_cur == Some(i);
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 54.0), Sense::click());
            if active {
                ui.painter().rect_filled(rect, CornerRadius::same(6), ACCENT.linear_multiply(0.14));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(6), pal.hover);
            }
            ui.painter().text(rect.left_top() + vec2(8.0, 4.0), egui::Align2::LEFT_TOP, truncate(&h.chapter, 50), egui::FontId::proportional(11.0), pal.muted);
            let chars: Vec<char> = h.excerpt.chars().collect();
            let a: String = chars[..h.mstart.min(chars.len())].iter().collect();
            let m: String = chars[h.mstart.min(chars.len())..(h.mstart + h.len).min(chars.len())].iter().collect();
            let b: String = chars[(h.mstart + h.len).min(chars.len())..].iter().collect();
            let mut job = egui::text::LayoutJob::default();
            let f = egui::FontId::proportional(13.0);
            job.append(&a, 0.0, egui::TextFormat::simple(f.clone(), pal.fg));
            job.append(&m, 0.0, egui::TextFormat { background: Color32::from_rgba_unmultiplied(255, 170, 0, 110), ..egui::TextFormat::simple(f.clone(), pal.fg) });
            job.append(&b, 0.0, egui::TextFormat::simple(f, pal.fg));
            job.wrap = egui::text::TextWrapping { max_width: w - 16.0, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
            let g = ui.painter().layout_job(job);
            ui.painter().galley(rect.left_top() + vec2(8.0, 20.0), g, pal.fg);
            if resp.clicked() {
                jump = Some(i);
            }
        }
    });
    if let Some(i) = jump {
        let l = r.search_hits[i].loc;
        r.jump(l, true);
        r.search_cur = Some(i);
    }
}

// ---------- settings ----------

pub(super) fn settings_window(r: &mut Reader, ctx: &egui::Context, open: &mut bool, env: &mut Env, actions: &mut Vec<Action>) {
    let per_book = r.data.settings.is_some();
    let mut s: ReaderSettings = r.effective(&env.app.reader);
    let before = s.clone();
    let fixed = r.book.fixed().is_some();
    egui::Window::new("Reading settings")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_TOP, vec2(-12.0, 56.0))
        .default_width(340.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() - 140.0).show(ui, |ui| {
                ui.set_width(330.0);
                ui::section_label(ui, "Theme");
                ui.horizontal_wrapped(|ui| {
                    for t in THEMES {
                        let p = ui::palette_for(*t, s.custom_bg, s.custom_fg);
                        let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
                        ui.painter().rect_filled(rect, CornerRadius::same(18), p.bg);
                        let sel = s.theme == *t;
                        ui.painter().rect_stroke(rect, CornerRadius::same(18), Stroke::new(if sel { 2.5 } else { 1.0 }, if sel { ACCENT } else { p.border.lerp_to_gamma(Color32::GRAY, 0.5) }), egui::StrokeKind::Inside);
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Aa", egui::FontId::new(13.0, egui::FontFamily::Name("book".into())), p.fg);
                        if resp.on_hover_text(t.name()).clicked() {
                            s.theme = *t;
                        }
                    }
                });
                if s.theme == Theme::Custom {
                    ui.horizontal(|ui| {
                        ui.label("Background");
                        ui.color_edit_button_srgb(&mut s.custom_bg);
                        ui.label("Text");
                        ui.color_edit_button_srgb(&mut s.custom_fg);
                    });
                }
                if !fixed {
                    ui::section_label(ui, "Font");
                    let current = if s.font.is_empty() { format!("Default ({})", env.installed.book_family) } else { s.font.clone() };
                    egui::ComboBox::from_id_salt("font").selected_text(current).width(300.0).height(360.0).show_ui(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut r.font_filter).hint_text("Filter fonts…"));
                        if ui.selectable_label(s.font.is_empty(), "Default serif").clicked() {
                            s.font.clear();
                        }
                        let f = r.font_filter.to_lowercase();
                        for fam in env.families.iter().filter(|x| f.is_empty() || x.to_lowercase().contains(&f)).take(400) {
                            if ui.selectable_label(&s.font == fam, egui::RichText::new(fam)).clicked() {
                                s.font = fam.clone();
                            }
                        }
                        if env.families.is_empty() {
                            ui.label("Scanning system fonts…");
                        }
                    });
                    slider(ui, "Size", &mut s.font_size, 12.0..=44.0, 0.5, "");
                    slider(ui, "Line spacing", &mut s.line_height, 1.0..=2.4, 0.05, "");
                    slider(ui, "Paragraph spacing", &mut s.para_spacing, 0.0..=2.0, 0.1, " em");
                    slider(ui, "Letter spacing", &mut s.letter_spacing, -1.0..=3.0, 0.1, " px");
                    ui.checkbox(&mut s.justify, "Justify text");
                    ui.checkbox(&mut s.indent, "Indent paragraphs");
                    ui.checkbox(&mut s.override_book, "Override book formatting").on_hover_text("Ignore the book's own font sizes and paragraph alignment");

                    ui::section_label(ui, "Layout");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.layout, Layout::Paginated, "Pages");
                        ui.selectable_value(&mut s.layout, Layout::Scrolled, "Scroll");
                        ui.separator();
                        ui.add_enabled_ui(s.layout == Layout::Paginated, |ui| {
                            ui.selectable_value(&mut s.columns, Columns::Auto, "Auto");
                            ui.selectable_value(&mut s.columns, Columns::One, "1 col");
                            ui.selectable_value(&mut s.columns, Columns::Two, "2 col");
                        });
                    });
                    slider(ui, "Margins", &mut s.margin, 0.0..=200.0, 2.0, " px");
                    slider(ui, "Max line width", &mut s.max_width, 360.0..=1800.0, 10.0, " px");
                    ui.checkbox(&mut s.show_header, "Show chapter title");
                    ui.checkbox(&mut s.show_footer, "Show progress footer");
                    ui.checkbox(&mut s.wheel_turns_page, "Mouse wheel turns pages");
                } else {
                    ui::section_label(ui, "Page");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut r.fx.zoom, 0, "Fit page");
                        ui.selectable_value(&mut r.fx.zoom, 1, "Fit width");
                    });
                    ui.horizontal(|ui| {
                        ui.label(format!("Zoom {:.0}%", r.fx.scale.max(0.01) * 100.0));
                        if icon_button(ui, Icon::ZoomOut, "Zoom out").clicked() {
                            r.fx.scale = (r.fx.scale / 1.15).max(0.3);
                        }
                        if icon_button(ui, Icon::ZoomIn, "Zoom in").clicked() {
                            r.fx.scale = (r.fx.scale * 1.15).min(5.0);
                        }
                        if ui.small_button("Reset").clicked() {
                            r.fx.scale = 1.0;
                        }
                    });
                    ui.checkbox(&mut r.fx.rtl, "Right-to-left (manga)");
                }
                ui.checkbox(&mut s.tap_zones, "Click page edges to turn pages");
                ui.checkbox(&mut s.dim_images, "Dim images in dark themes");

                ui.separator();
                let mut pb = per_book;
                if ui.checkbox(&mut pb, "Use these settings for this book only").changed() {
                    r.data.settings = if pb { Some(s.clone()) } else { None };
                    r.dirty = true;
                }
                if ui.button("Reset to defaults").clicked() {
                    let fresh = ReaderSettings { theme: s.theme, ..Default::default() };
                    s = fresh;
                }
            });
        });
    if s != before {
        if s.font != before.font {
            actions.push(Action::InstallFont { family: s.font.clone(), cjk: env.installed.cjk });
        }
        if r.data.settings.is_some() {
            r.data.settings = Some(s);
            r.dirty = true;
        } else {
            env.app.reader = s;
        }
    }
}

fn slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, suffix: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(118.0, 20.0), egui::Label::new(label));
        ui.spacing_mut().slider_width = 140.0;
        ui.add(egui::Slider::new(v, range).step_by(step).suffix(suffix));
    });
}
