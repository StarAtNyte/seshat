//! Sidebar (contents / notes / bookmarks / search) and the reading settings panel.

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense, Stroke};

use super::{Action, Env, Reader, Side};
use crate::book::{Content, Loc};
use crate::store::{self, Columns, Layout, ReaderSettings, Theme, THEMES};
use crate::ui::{self, icon_button, Icon, Palette, ACCENT};

pub(super) fn sidebar(r: &mut Reader, ctx: &egui::Context, side: Side, pal: &Palette, env: &mut Env) {
    egui::SidePanel::left("reader-side")
        .resizable(true)
        .default_width(264.0)
        .width_range(220.0..=380.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(ui::desk(pal)).inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 12 }))
        .show(ctx, |ui| {
            // header: title/author flush-left with the lists, close flush-right
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 34.0);
                    ui.add(egui::Label::new(RichText::new(&r.book.meta.title).font(ui::ui_b(16.0))).truncate());
                    if !r.book.meta.author.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&r.book.meta.author).font(ui::ui_m(12.5)).color(pal.muted)).truncate());
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if icon_button(ui, Icon::Close, "Close (C)").clicked() {
                        r.side = None;
                    }
                });
            });
            ui.add_space(12.0);
            // four equal tabs with a sliding underline
            let tabs = [(Side::Toc, "Contents", Icon::List), (Side::Notes, "Notes", Icon::Note), (Side::Bookmarks, "Marks", Icon::Bookmark), (Side::Search, "Find", Icon::Search)];
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
            let cw = row.width() / tabs.len() as f32;
            let mut active_x = row.left();
            for (i, (s, label, icon)) in tabs.iter().enumerate() {
                let cell = egui::Rect::from_min_size(egui::pos2(row.left() + i as f32 * cw, row.top()), vec2(cw, 46.0));
                let resp = ui.interact(cell, ui.id().with(("tab", i)), Sense::click());
                let on = side == *s;
                if on {
                    active_x = cell.left();
                }
                let col = if on { ACCENT } else if resp.hovered() { pal.fg } else { pal.muted };
                ui::paint_icon(ui.painter(), egui::Rect::from_center_size(cell.center() - vec2(0.0, 8.0), vec2(16.0, 16.0)), *icon, col);
                let count = match s {
                    Side::Notes => r.data.annotations.len(),
                    Side::Bookmarks => r.data.bookmarks.len(),
                    Side::Search => r.search_hits.len(),
                    _ => 0,
                };
                let text = if count > 0 { format!("{label} {count}") } else { label.to_string() };
                ui.painter().text(cell.center() + vec2(0.0, 11.0), egui::Align2::CENTER_CENTER, text, ui::ui_m(11.5), col);
                if resp.clicked() {
                    r.side = Some(*s);
                    r.focus_search = *s == Side::Search;
                    r.toc_scroll = *s == Side::Toc;
                }
            }
            ui.painter().hline(row.x_range(), row.bottom(), Stroke::new(1.0, pal.border));
            let ax = ctx.animate_value_with_time(ui.id().with("tab-x"), active_x, 0.18);
            ui.painter().hline(ax + 10.0..=ax + cw - 10.0, row.bottom(), Stroke::new(2.0, ACCENT));
            ui.add_space(10.0);
            match side {
                Side::Toc => toc(r, ui, pal),
                Side::Notes => notes(r, ui, pal),
                Side::Bookmarks => bookmarks(r, ui, pal),
                Side::Search => search(r, ui, pal, env),
            }
        });
}

/// Right-hand notebook: this chapter's highlights and notes in reading order,
/// plus a quick page note.
pub(super) fn notebook(r: &mut Reader, ctx: &egui::Context, pal: &Palette) {
    egui::SidePanel::right("reader-notebook")
        .resizable(true)
        .default_width(280.0)
        .width_range(230.0..=400.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(ui::desk(pal)).inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 12 }))
        .show(ctx, |ui| {
            let cur = cur_loc(r);
            let chapter = r.chapter_title(cur);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 34.0);
                    ui.label(RichText::new("Notebook").font(ui::ui_b(16.0)));
                    ui.add(egui::Label::new(RichText::new(&chapter).font(ui::ui_m(12.5)).color(pal.muted)).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if icon_button(ui, Icon::Close, "Close (N)").clicked() {
                        r.notebook = false;
                    }
                });
            });
            ui.add_space(12.0);
            // quick note pinned to the current page
            let te = ui.add(egui::TextEdit::multiline(&mut r.quick_note).hint_text("Jot something about this page…").desired_rows(3).desired_width(f32::INFINITY));
            let send = te.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Ctrl+Enter to save").font(ui::ui_m(11.5)).color(pal.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if (ui::text_button(ui, "Save note", false).clicked() || send) && !r.quick_note.trim().is_empty() {
                        r.add_page_note();
                    }
                });
            });
            ui.add_space(8.0);
            ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0, pal.border));
            ui.add_space(10.0);
            let mine: Vec<store::Annotation> = r.data.annotations.iter().filter(|a| a.start.section == cur.section).cloned().collect();
            if mine.is_empty() {
                ui.label(RichText::new("Nothing marked in this chapter yet. Select text to highlight, or press H for the highlighter.").font(ui::ui_m(12.5)).color(pal.muted));
                return;
            }
            let mut jump = None;
            let mut edit = None;
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for a in &mine {
                    let [cr, cg, cb] = a.color.rgb();
                    let top = ui.cursor().top();
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            if !a.text.is_empty() {
                                if ui.add(egui::Label::new(RichText::new(truncate(&a.text, 260)).family(egui::FontFamily::Name("book-i".into())).size(14.0)).sense(Sense::click())).clicked() {
                                    jump = Some(a.start);
                                }
                            }
                            if !a.note.is_empty() {
                                ui.label(RichText::new(&a.note).font(ui::ui_m(13.0)).color(ACCENT));
                            }
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(store::relative_time(a.created)).font(ui::ui_m(11.0)).color(pal.muted));
                                if ui::text_button(ui, if a.note.is_empty() { "Add note" } else { "Edit" }, false).clicked() {
                                    edit = Some((a.id, a.note.clone()));
                                }
                            });
                        });
                    });
                    let bottom = ui.cursor().top();
                    ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), top + 2.0), egui::pos2(ui.max_rect().left() + 3.0, bottom - 6.0)), CornerRadius::same(1), Color32::from_rgb(cr, cg, cb));
                    ui.add_space(8.0);
                }
            });
            if let Some(l) = jump {
                r.jump(l, true);
            }
            if edit.is_some() {
                r.note_edit = edit;
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
            // the pill reaches into the margin so the text stays flush with the header
            let pill = rect.expand2(vec2(8.0, 0.0));
            if active {
                ui.painter().rect_filled(pill, CornerRadius::same(6), ACCENT.linear_multiply(0.14));
            } else if resp.hovered() {
                ui.painter().rect_filled(pill, CornerRadius::same(6), pal.hover);
            }
            let indent = e.depth as f32 * 16.0;
            let font = egui::FontId::proportional(if e.depth == 0 { 14.0 } else { 13.5 });
            let color = if active { ACCENT } else if e.depth == 0 { pal.fg } else { pal.muted.lerp_to_gamma(pal.fg, 0.4) };
            let galley = ui.painter().layout(e.title.clone(), font, color, w - indent - 50.0);
            let g = if galley.rows.len() > 1 { ui.painter().layout_no_wrap(truncate(&e.title, ((w - indent - 50.0) / 7.0) as usize), egui::FontId::proportional(13.5), color) } else { galley };
            ui.painter().galley(rect.left_center() + vec2(indent, -g.size().y / 2.0), g, color);
            if let Content::Flow(f) = &book.content {
                let pct = (f.fraction(e.loc) * 100.0).round();
                ui.painter().text(rect.right_center(), egui::Align2::RIGHT_CENTER, format!("{pct}%"), egui::FontId::proportional(11.0), pal.muted);
            } else {
                ui.painter().text(rect.right_center(), egui::Align2::RIGHT_CENTER, format!("{}", e.loc.section + 1), egui::FontId::proportional(11.0), pal.muted);
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

/// Reading settings as a calm right-hand drawer with four tabs.
pub(super) fn settings_window(r: &mut Reader, ctx: &egui::Context, open: &mut bool, env: &mut Env, actions: &mut Vec<Action>) {
    let per_book = r.data.settings.is_some();
    let mut s: ReaderSettings = r.effective(&env.app.reader);
    let before = s.clone();
    let pal = ui::palette(&s);
    let fixed = r.book.fixed().is_some();
    egui::SidePanel::right("reader-settings")
        .resizable(false)
        .exact_width(336.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(ui::desk(&pal)).inner_margin(egui::Margin { left: 20, right: 20, top: 16, bottom: 16 }))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Reading settings").font(ui::ui_b(17.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, Icon::Close, "Close").clicked() {
                        *open = false;
                    }
                });
            });
            ui.add_space(14.0);
            // tabs
            let tabs = ["Text", "Layout", "Theme", "More"];
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            let cw = row.width() / tabs.len() as f32;
            for (i, t) in tabs.iter().enumerate() {
                let cell = egui::Rect::from_min_size(egui::pos2(row.left() + i as f32 * cw, row.top()), vec2(cw, 34.0));
                let resp = ui.interact(cell, ui.id().with(("stab", i)), Sense::click());
                let on = r.settings_tab as usize == i;
                ui.painter().text(cell.center(), egui::Align2::CENTER_CENTER, *t, ui::ui_m(13.5), if on { ACCENT } else if resp.hovered() { pal.fg } else { pal.muted });
                if resp.clicked() {
                    r.settings_tab = i as u8;
                }
            }
            ui.painter().hline(row.x_range(), row.bottom(), Stroke::new(1.0, pal.border));
            let ax = ctx.animate_value_with_time(ui.id().with("stab-x"), row.left() + r.settings_tab as f32 * cw, 0.18);
            ui.painter().hline(ax + 12.0..=ax + cw - 12.0, row.bottom(), Stroke::new(2.0, ACCENT));
            ui.add_space(18.0);

            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                match r.settings_tab {
                    0 if !fixed => {
                        group(ui, &pal, "Typeface");
                        let current = if s.font.is_empty() { format!("Default · {}", env.installed.book_family) } else { s.font.clone() };
                        egui::ComboBox::from_id_salt("font").selected_text(current).width(ui.available_width()).height(360.0).show_ui(ui, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut r.font_filter).hint_text("Filter fonts…"));
                            if ui.selectable_label(s.font.is_empty(), "Default serif").clicked() {
                                s.font.clear();
                            }
                            let f = r.font_filter.to_lowercase();
                            for fam in env.families.iter().filter(|x| f.is_empty() || x.to_lowercase().contains(&f)).take(400) {
                                if ui.selectable_label(&s.font == fam, fam.as_str()).clicked() {
                                    s.font = fam.clone();
                                }
                            }
                            if env.families.is_empty() {
                                ui.label("Scanning system fonts…");
                            }
                        });
                        ui.label(RichText::new("The quick brown fox jumps over the lazy dog.").family(egui::FontFamily::Name("book".into())).size(s.font_size.min(22.0)));
                        group(ui, &pal, "Size & spacing");
                        row_slider(ui, &pal, "Size", &mut s.font_size, 12.0..=44.0, 0.5, "");
                        row_slider(ui, &pal, "Line height", &mut s.line_height, 1.0..=2.4, 0.05, "");
                        row_slider(ui, &pal, "Paragraphs", &mut s.para_spacing, 0.0..=2.0, 0.1, " em");
                        row_slider(ui, &pal, "Letters", &mut s.letter_spacing, -1.0..=3.0, 0.1, " px");
                        group(ui, &pal, "Paragraphs");
                        ui.checkbox(&mut s.justify, "Justify text");
                        ui.checkbox(&mut s.indent, "Indent first lines");
                        ui.checkbox(&mut s.override_book, "Ignore the book's own formatting");
                    }
                    0 | 1 if fixed => {
                        group(ui, &pal, "Page");
                        if let Some(i) = ui::segmented(ui, "s-zoom", &["Fit page", "Fit width"], r.fx.zoom as usize) {
                            r.fx.zoom = i as u8;
                        }
                        row_slider(ui, &pal, "Zoom", &mut r.fx.scale, 0.3..=5.0, 0.05, "×");
                        ui.checkbox(&mut r.fx.rtl, "Right-to-left page order (manga)");
                    }
                    1 => {
                        group(ui, &pal, "View");
                        let sel = match (s.layout, s.columns) {
                            (Layout::Scrolled, _) => 2,
                            (_, Columns::One) => 0,
                            _ => 1,
                        };
                        if let Some(i) = ui::segmented(ui, "s-layout", &["Single page", "Spread", "Scroll"], sel) {
                            match i {
                                0 => {
                                    s.layout = Layout::Paginated;
                                    s.columns = Columns::One;
                                }
                                1 => {
                                    s.layout = Layout::Paginated;
                                    s.columns = Columns::Two;
                                }
                                _ => s.layout = Layout::Scrolled,
                            }
                        }
                        group(ui, &pal, "Page");
                        row_slider(ui, &pal, "Margins", &mut s.margin, 0.0..=200.0, 2.0, " px");
                        row_slider(ui, &pal, "Line width", &mut s.max_width, 360.0..=1800.0, 10.0, " px");
                        group(ui, &pal, "Around the text");
                        ui.checkbox(&mut s.show_header, "Running heads (book and chapter)");
                        ui.checkbox(&mut s.show_footer, "Page numbers and time left");
                    }
                    2 => {
                        group(ui, &pal, "Theme");
                        let w = (ui.available_width() - 30.0) / 4.0;
                        egui::Grid::new("themes").spacing(vec2(10.0, 10.0)).show(ui, |ui| {
                            for (i, t) in THEMES.iter().enumerate() {
                                let p = ui::palette_for(*t, s.custom_bg, s.custom_fg);
                                let (rr, resp) = ui.allocate_exact_size(vec2(w, 64.0), Sense::click());
                                let on = s.theme == *t;
                                let k = ctx.animate_bool_with_time(resp.id, resp.hovered() || on, 0.12);
                                ui.painter().rect_filled(rr.shrink(2.0 - 2.0 * k), CornerRadius::same(8), p.bg);
                                ui.painter().rect_stroke(rr.shrink(2.0 - 2.0 * k), CornerRadius::same(8), Stroke::new(if on { 2.0 } else { 1.0 }, if on { ACCENT } else { pal.border }), egui::StrokeKind::Inside);
                                ui.painter().text(rr.center() - vec2(0.0, 7.0), egui::Align2::CENTER_CENTER, "Aa", egui::FontId::new(16.0, egui::FontFamily::Name("book".into())), p.fg);
                                ui.painter().text(rr.center() + vec2(0.0, 16.0), egui::Align2::CENTER_CENTER, t.name(), ui::ui_m(10.5), p.muted);
                                if resp.clicked() {
                                    s.theme = *t;
                                }
                                if i % 4 == 3 {
                                    ui.end_row();
                                }
                            }
                        });
                        if s.theme == Theme::Custom {
                            group(ui, &pal, "Custom colours");
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Paper").color(pal.muted));
                                ui.color_edit_button_srgb(&mut s.custom_bg);
                                ui.add_space(16.0);
                                ui.label(RichText::new("Ink").color(pal.muted));
                                ui.color_edit_button_srgb(&mut s.custom_fg);
                            });
                        }
                        group(ui, &pal, "Images");
                        ui.checkbox(&mut s.dim_images, "Dim images in dark themes");
                    }
                    _ => {
                        group(ui, &pal, "Motion");
                        if let Some(i) = ui::segmented(ui, "s-motion", &["Full", "Simple", "Off"], s.motion as usize) {
                            s.motion = i as u8;
                        }
                        ui.label(RichText::new(["The cover opens and pages curl over the spine.", "Pages cross-fade quickly.", "Pages change instantly."][s.motion.min(2) as usize]).font(ui::ui_m(12.0)).color(pal.muted));
                        group(ui, &pal, "Turning pages");
                        ui.checkbox(&mut s.tap_zones, "Click the page edges to turn");
                        ui.checkbox(&mut s.wheel_turns_page, "Mouse wheel turns pages");
                        group(ui, &pal, "Auto-advance (A)");
                        row_slider(ui, &pal, "Seconds/page", &mut s.auto_secs, 5.0..=120.0, 1.0, " s");
                        row_slider(ui, &pal, "Scroll speed", &mut s.auto_px, 5.0..=200.0, 1.0, " px/s");
                        group(ui, &pal, "Applies to");
                        let mut pb = per_book as usize;
                        if let Some(i) = ui::segmented(ui, "s-scope", &["All books", "This book"], pb) {
                            pb = i;
                            r.data.settings = if pb == 1 { Some(s.clone()) } else { None };
                            r.dirty = true;
                        }
                        ui.add_space(8.0);
                        if ui::text_button(ui, "Reset to defaults", false).clicked() {
                            s = ReaderSettings { theme: s.theme, ..Default::default() };
                        }
                    }
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

fn group(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).font(ui::ui_m(12.0)).color(pal.muted));
}

fn row_slider(ui: &mut egui::Ui, pal: &Palette, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, suffix: &str) {
    ui.horizontal(|ui| {
        let (lr, _) = ui.allocate_exact_size(vec2(92.0, 24.0), Sense::hover());
        ui.painter().text(lr.left_center(), egui::Align2::LEFT_CENTER, label, ui::ui_m(13.0), pal.fg);
        ui.spacing_mut().slider_width = (ui.available_width() - 78.0).max(80.0);
        ui.spacing_mut().interact_size.x = 70.0;
        ui.add(egui::Slider::new(v, range).step_by(step).suffix(suffix));
    });
}
