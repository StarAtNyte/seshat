//! Sidebar (contents / notes / bookmarks / search) and the reading settings panel.

use crate::i18n::tr;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, RichText, Sense, Stroke};

use super::{Action, Env, Reader, Side};
use crate::book::{Content, Loc};
use crate::store::{self, Columns, Layout, ReaderSettings, Theme, THEMES};
use crate::ui::{self, icon_button, Icon, Palette, ACCENT};

pub(super) fn sidebar(r: &mut Reader, ctx: &egui::Context, side: Side, pal: &Palette, env: &mut Env) {
    egui::SidePanel::left("reader-side-v2")
        .resizable(true)
        .default_width(330.0)
        .width_range(260.0..=480.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(ui::desk(pal)).inner_margin(egui::Margin { left: 20, right: 20, top: 18, bottom: 16 }))
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
                    if icon_button(ui, Icon::Close, tr("Close (C)")).clicked() {
                        r.side = None;
                    }
                });
            });
            ui.add_space(16.0);
            // four equal tabs with a sliding underline
            let tabs = [(Side::Toc, tr("Contents"), Icon::List), (Side::Notes, tr("Notes"), Icon::Note), (Side::Bookmarks, tr("Marks"), Icon::Bookmark), (Side::Search, tr("Find"), Icon::Search)];
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
            ui.add_space(16.0);
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
        .frame(egui::Frame::new().fill(ui::desk(pal)).inner_margin(egui::Margin { left: 20, right: 20, top: 18, bottom: 16 }))
        .show(ctx, |ui| {
            let cur = cur_loc(r);
            let chapter = r.chapter_title(cur);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 34.0);
                    ui.label(RichText::new(tr("Notebook")).font(ui::ui_b(16.0)));
                    ui.add(egui::Label::new(RichText::new(&chapter).font(ui::ui_m(12.5)).color(pal.muted)).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if icon_button(ui, Icon::Close, tr("Close (N)")).clicked() {
                        r.notebook = false;
                    }
                });
            });
            ui.add_space(16.0);
            // quick note pinned to the current page
            let te = ui.add(egui::TextEdit::multiline(&mut r.quick_note).hint_text(tr("Jot something about this page…")).font(ui::ui_m(14.0)).margin(egui::Margin::symmetric(12, 10)).desired_rows(3).desired_width(f32::INFINITY));
            let send = te.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Ctrl+Enter to save")).font(ui::ui_m(11.5)).color(pal.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if (ui::text_button(ui, tr("Save note"), false).clicked() || send) && !r.quick_note.trim().is_empty() {
                        r.add_page_note();
                    }
                });
            });
            ui.add_space(14.0);
            ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0, pal.border));
            ui.add_space(16.0);
            let mine: Vec<store::Annotation> = r.data.annotations.iter().filter(|a| a.start.section == cur.section).cloned().collect();
            if mine.is_empty() {
                ui.label(RichText::new(tr("Nothing marked in this chapter yet. Select text to highlight, or press H for the highlighter.")).font(ui::ui_m(12.5)).color(pal.muted));
                return;
            }
            let mut jump = None;
            let mut edit = None;
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for a in &mine {
                    let [cr, cg, cb] = a.color.rgb();
                    card(ui, pal, Some(Color32::from_rgb(cr, cg, cb)), |ui| {
                        if !a.text.is_empty() && ui.add(egui::Label::new(RichText::new(truncate(&a.text, 260)).family(egui::FontFamily::Name("book-i".into())).size(14.0).color(pal.fg)).wrap().sense(Sense::click())).clicked() {
                            jump = Some(a.start);
                        }
                        if !a.note.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&a.note).font(ui::ui_m(13.0)).color(ACCENT)).wrap());
                        }
                        card_footer(ui, pal, &store::relative_time(a.created), |ui| {
                            if ui::text_button(ui, if a.note.is_empty() { tr("Add note") } else { tr("Edit") }, false).clicked() {
                                edit = Some((a.id, a.note.clone()));
                            }
                        });
                    });
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

// ---------- shared sidebar pieces ----------

/// A roomy text field: 38 px tall, padded, full width.
fn field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> egui::Response {
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).font(ui::ui_m(14.0)).margin(egui::Margin::symmetric(12, 10)).desired_width(f32::INFINITY))
}

/// Muted explanatory line (empty states, hints), wrapped.
fn note_text(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(ui::ui_m(13.0)).color(pal.muted)).wrap());
}

/// Small heading over a group of cards (a chapter name).
fn group_head(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new(text).font(ui::ui_m(12.0)).color(pal.muted)).truncate());
    ui.add_space(6.0);
}

/// A card with an optional colour stripe; full width, even padding.
fn card<R>(ui: &mut egui::Ui, pal: &Palette, stripe: Option<Color32>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = egui::Frame::new()
        .fill(pal.hover)
        .corner_radius(10)
        .inner_margin(egui::Margin { left: if stripe.is_some() { 16 } else { 12 }, right: 12, top: 10, bottom: 8 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            let out = add(ui);
            // the stripe spans the content actually laid out, not the space left
            if let Some(c) = stripe {
                let rect = ui.min_rect();
                ui.painter().rect_filled(egui::Rect::from_min_size(rect.left_top() - vec2(10.0, 0.0), vec2(3.0, rect.height())), CornerRadius::same(2), c);
            }
            out
        })
        .inner;
    ui.add_space(8.0);
    r
}

/// Footer of a card: muted meta text on the left, actions on the right.
fn card_footer(ui: &mut egui::Ui, pal: &Palette, meta: &str, actions: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(meta).font(ui::ui_m(11.5)).color(pal.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            actions(ui)
        });
    });
}

/// A row of actions above a list, followed by even spacing.
fn toolbar(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        add(ui)
    });
    ui.add_space(12.0);
}

fn toc(r: &mut Reader, ui: &mut egui::Ui, pal: &Palette) {
    let cur = r.book.toc_index_for(cur_loc(r));
    if r.book.toc.is_empty() {
        note_text(ui, pal, tr("This book has no table of contents."));
        return;
    }
    let mut jump = None;
    let scroll_now = std::mem::take(&mut r.toc_scroll);
    let fracs = r.toc_fractions().to_vec();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let book = r.book.clone();
        for (i, e) in book.toc.iter().enumerate() {
            let active = cur == Some(i);
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
            if active {
                ui.painter().rect_filled(rect, CornerRadius::same(8), ACCENT.linear_multiply(0.14));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(8), pal.hover);
            }
            let indent = 10.0 + e.depth.min(4) as f32 * 16.0;
            let font = ui::ui_m(if e.depth == 0 { 14.0 } else { 13.5 });
            let color = if active { ACCENT } else if e.depth == 0 { pal.fg } else { pal.muted.lerp_to_gamma(pal.fg, 0.45) };
            let right = if let (Content::Flow(_), Some(fr)) = (&book.content, fracs.get(i)) { format!("{}%", (fr * 100.0).round()) } else { format!("{}", e.loc.section + 1) };
            let rw = 44.0;
            let title = ui::fit(ui, &crate::book::script::visual(&e.title), font.clone(), w - indent - rw - 10.0);
            ui.painter().text(pos2(rect.left() + indent, rect.center().y), egui::Align2::LEFT_CENTER, title, font, color);
            ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, right, ui::ui_m(11.5), if active { ACCENT } else { pal.muted });
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
        note_text(ui, pal, tr("Select text in the book to highlight it or add a note."));
        return;
    }
    toolbar(ui, |ui| {
        if ui::text_button(ui, tr("Export Markdown…"), false).clicked() {
            export_notes(r);
        }
        if ui::text_button(ui, tr("Copy all"), false).clicked() {
            ui.ctx().copy_text(notes_markdown(r));
        }
    });
    let mut jump = None;
    let mut delete = None;
    let mut edit = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let mut last_chapter = String::new();
        for a in &r.data.annotations {
            if a.chapter != last_chapter {
                group_head(ui, pal, &a.chapter);
                last_chapter = a.chapter.clone();
            }
            let [cr, cg, cb] = a.color.rgb();
            card(ui, pal, Some(Color32::from_rgb(cr, cg, cb)), |ui| {
                let resp = ui.add(egui::Label::new(RichText::new(truncate(&a.text, 300)).font(ui::ui_m(13.5)).color(pal.fg)).wrap().sense(Sense::click()));
                if !a.note.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&a.note).font(ui::ui_m(13.0)).italics().color(ACCENT)).wrap());
                }
                card_footer(ui, pal, &store::relative_time(a.created), |ui| {
                    if ui::text_button(ui, tr("Delete"), false).clicked() {
                        delete = Some(a.id);
                    }
                    if ui::text_button(ui, if a.note.is_empty() { tr("Add note") } else { tr("Edit note") }, false).clicked() {
                        edit = Some((a.id, a.note.clone()));
                    }
                });
                if resp.clicked() {
                    jump = Some(a.start);
                }
            });
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
    toolbar(ui, |ui| {
        if ui::text_button(ui, if r.bookmark_here().is_some() { tr("Remove bookmark on this page") } else { tr("Bookmark this page") }, false).clicked() {
            r.toggle_bookmark();
        }
    });
    if r.data.bookmarks.is_empty() {
        note_text(ui, pal, tr("No bookmarks yet. Press Ctrl+B to bookmark the current page."));
        return;
    }
    let mut jump = None;
    let mut del = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (i, b) in r.data.bookmarks.iter().enumerate() {
            card(ui, pal, Some(ACCENT), |ui| {
                ui.add(egui::Label::new(RichText::new(&b.chapter).font(ui::ui_m(12.0)).color(pal.muted)).truncate());
                if ui.add(egui::Label::new(RichText::new(truncate(&b.excerpt, 140)).font(ui::ui_m(13.5)).color(pal.fg)).wrap().sense(Sense::click())).clicked() {
                    jump = Some(b.loc);
                }
                card_footer(ui, pal, &store::relative_time(b.created), |ui| {
                    if ui::text_button(ui, tr("Remove"), false).clicked() {
                        del = Some(i);
                    }
                });
            });
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
    let resp = field(ui, &mut r.search_q, tr("Search in book…"));
    if std::mem::take(&mut r.focus_search) {
        resp.request_focus();
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        r.run_search();
        resp.request_focus();
    }
    ui.add_space(10.0);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    {
        let ui = &mut child;
        if r.search_running {
            ui.spinner();
            ui.label(RichText::new(trf!("{n} results so far…", n = r.search_hits.len())).font(ui::ui_m(12.5)).color(pal.muted));
        } else if !r.search_hits.is_empty() {
            let pos = r.search_cur.map(|i| format!("{} / ", i + 1)).unwrap_or_default();
            ui.label(RichText::new(format!("{pos}{} results", r.search_hits.len())).font(ui::ui_m(12.5)).color(pal.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui::text_button(ui, "▶", false).on_hover_text(tr("Next result")).clicked() {
                    let i = r.search_cur.map(|i| (i + 1).min(r.search_hits.len() - 1)).unwrap_or(0);
                    let l = r.search_hits[i].loc;
                    r.jump(l, false);
                    r.search_cur = Some(i);
                }
                if ui::text_button(ui, "◀", false).on_hover_text(tr("Previous result")).clicked() {
                    let i = r.search_cur.map(|i| i.saturating_sub(1)).unwrap_or(0);
                    let l = r.search_hits[i].loc;
                    r.jump(l, false);
                    r.search_cur = Some(i);
                }
            });
        } else if r.search_q.trim().chars().count() >= 2 && r.search_gen > 0 {
            ui.label(RichText::new(tr("No results")).font(ui::ui_m(12.5)).color(pal.muted));
        } else {
            ui.label(RichText::new(tr("Press Enter to search")).font(ui::ui_m(12.5)).color(pal.muted));
        }
    }
    if r.book.fixed().is_some() && r.fx.texts.is_none() && r.search_running {
        note_text(ui, pal, tr("Extracting PDF text (first search only)…"));
    }
    ui.add_space(8.0);
    ui::rule(ui);
    ui.add_space(8.0);
    let mut jump = None;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, 66.0, r.search_hits.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for i in range {
            let h = &r.search_hits[i];
            let active = r.search_cur == Some(i);
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 62.0), Sense::click());
            if active {
                ui.painter().rect_filled(rect, CornerRadius::same(8), ACCENT.linear_multiply(0.14));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(8), pal.hover);
            }
            ui.painter().text(rect.left_top() + vec2(10.0, 8.0), egui::Align2::LEFT_TOP, ui::fit(ui, &h.chapter, ui::ui_m(11.5), w - 20.0), ui::ui_m(11.5), if active { ACCENT } else { pal.muted });
            let chars: Vec<char> = h.excerpt.chars().collect();
            let a: String = chars[..h.mstart.min(chars.len())].iter().collect();
            let m: String = chars[h.mstart.min(chars.len())..(h.mstart + h.len).min(chars.len())].iter().collect();
            let b: String = chars[(h.mstart + h.len).min(chars.len())..].iter().collect();
            let mut job = egui::text::LayoutJob::default();
            let f = ui::ui_m(13.0);
            job.append(&a, 0.0, egui::TextFormat::simple(f.clone(), pal.fg));
            job.append(&m, 0.0, egui::TextFormat { background: Color32::from_rgba_unmultiplied(255, 170, 0, 110), ..egui::TextFormat::simple(f.clone(), pal.fg) });
            job.append(&b, 0.0, egui::TextFormat::simple(f, pal.fg));
            job.wrap = egui::text::TextWrapping { max_width: w - 20.0, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
            let g = ui.painter().layout_job(job);
            ui.painter().galley(rect.left_top() + vec2(10.0, 26.0), g, pal.fg);
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
        .exact_width(380.0)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(ui::desk(&pal)).inner_margin(egui::Margin { left: 20, right: 20, top: 18, bottom: 16 }))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Reading settings")).font(ui::ui_b(17.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, Icon::Close, tr("Close")).clicked() {
                        *open = false;
                    }
                });
            });
            ui.add_space(14.0);
            // tabs
            let tabs = [tr("Text"), tr("Layout"), tr("Theme"), tr("Voice"), tr("More")];
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            let cw = row.width() / tabs.len() as f32;
            for (i, t) in tabs.iter().enumerate() {
                let cell = egui::Rect::from_min_size(egui::pos2(row.left() + i as f32 * cw, row.top()), vec2(cw, 34.0));
                let resp = ui.interact(cell, ui.id().with(("stab", i)), Sense::click());
                let on = r.settings_tab as usize == i;
                ui::name(&resp, &format!("{t} settings"), Some(on));
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
                        group(ui, &pal, tr("Typeface"));
                        let current = if s.font.is_empty() { trf!("Default · {name}", name = env.installed.book_family) } else { s.font.clone() };
                        egui::ComboBox::from_id_salt("font").selected_text(current).width(ui.available_width()).height(360.0).show_ui(ui, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut r.font_filter).hint_text(tr("Filter fonts…")));
                            if ui.selectable_label(s.font.is_empty(), tr("Default serif")).clicked() {
                                s.font.clear();
                            }
                            let f = r.font_filter.to_lowercase();
                            for fam in env.families.iter().filter(|x| f.is_empty() || x.to_lowercase().contains(&f)).take(400) {
                                if ui.selectable_label(&s.font == fam, fam.as_str()).clicked() {
                                    s.font = fam.clone();
                                }
                            }
                            if env.families.is_empty() {
                                ui.label(tr("Scanning system fonts…"));
                            }
                        });
                        ui.label(RichText::new("The quick brown fox jumps over the lazy dog.").family(egui::FontFamily::Name("book".into())).size(s.font_size.min(22.0)));
                        group(ui, &pal, tr("Size & spacing"));
                        row_slider(ui, &pal, tr("Size"), &mut s.font_size, 12.0..=44.0, 0.5, "");
                        row_slider(ui, &pal, tr("Line height"), &mut s.line_height, 1.0..=2.4, 0.05, "");
                        row_slider(ui, &pal, tr("Paragraphs"), &mut s.para_spacing, 0.0..=2.0, 0.1, " em");
                        row_slider(ui, &pal, tr("Letters"), &mut s.letter_spacing, -1.0..=3.0, 0.1, " px");
                        group(ui, &pal, tr("Paragraphs"));
                        ui.checkbox(&mut s.justify, tr("Justify text"));
                        ui.checkbox(&mut s.indent, tr("Indent first lines"));
                        ui.checkbox(&mut s.override_book, tr("Ignore the book's own formatting"));
                    }
                    0 | 1 if fixed => {
                        group(ui, &pal, tr("Page"));
                        if let Some(i) = ui::segmented(ui, "s-zoom", &[tr("Fit page"), tr("Fit width")], r.fx.zoom as usize) {
                            r.fx.zoom = i as u8;
                        }
                        row_slider(ui, &pal, tr("Zoom"), &mut r.fx.scale, 0.3..=5.0, 0.05, "×");
                        ui.checkbox(&mut r.fx.rtl, tr("Right-to-left page order (manga)"));
                        let mut crop = r.fx.crop;
                        if ui.checkbox(&mut crop, tr("Crop margins")).changed() {
                            r.set_crop(crop);
                        }
                        if matches!(r.book.fixed().map(|f| &f.kind), Some(crate::book::FixedKind::Pdf)) {
                            let mut rf = r.fx.reflow;
                            if ui.checkbox(&mut rf, tr("Reflow text (reader font and theme)")).changed() {
                                r.set_reflow(rf);
                            }
                        }
                    }
                    1 => {
                        group(ui, &pal, tr("View"));
                        let sel = match (s.layout, s.columns) {
                            (Layout::Scrolled, _) => 2,
                            (_, Columns::One) => 0,
                            _ => 1,
                        };
                        if let Some(i) = ui::segmented(ui, "s-layout", &[tr("Single page"), tr("Spread"), tr("Scroll")], sel) {
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
                        group(ui, &pal, tr("Page"));
                        row_slider(ui, &pal, tr("Margins"), &mut s.margin, 0.0..=200.0, 2.0, " px");
                        row_slider(ui, &pal, tr("Line width"), &mut s.max_width, 360.0..=1800.0, 10.0, " px");
                        group(ui, &pal, tr("Around the text"));
                        ui.checkbox(&mut s.show_header, tr("Running heads (book and chapter)"));
                        ui.checkbox(&mut s.show_footer, tr("Page numbers and time left"));
                    }
                    2 => {
                        group(ui, &pal, tr("Theme"));
                        let w = (ui.available_width() - 30.0) / 4.0;
                        egui::Grid::new("themes").spacing(vec2(10.0, 10.0)).show(ui, |ui| {
                            for (i, t) in THEMES.iter().enumerate() {
                                let p = ui::palette_for(*t, s.custom_bg, s.custom_fg);
                                let (rr, resp) = ui.allocate_exact_size(vec2(w, 64.0), Sense::click());
                                let on = s.theme == *t;
                                ui::name(&resp, &format!("{} theme", t.name()), Some(on));
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
                            group(ui, &pal, tr("Custom colours"));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(tr("Paper")).color(pal.muted));
                                ui.color_edit_button_srgb(&mut s.custom_bg);
                                ui.add_space(16.0);
                                ui.label(RichText::new(tr("Ink")).color(pal.muted));
                                ui.color_edit_button_srgb(&mut s.custom_fg);
                            });
                        }
                        group(ui, &pal, tr("Images"));
                        ui.checkbox(&mut s.dim_images, tr("Dim images in dark themes"));
                    }
                    3 => {
                        group(ui, &pal, tr("Read aloud"));
                        ui.add_space(4.0);
                        crate::app::App::voice_settings(ui, env.app, &pal);
                        if r.speaker.is_some() {
                            ui::form_note(ui, tr("Changes apply when you start reading aloud again."));
                        }
                    }
                    _ => {
                        group(ui, &pal, tr("Motion"));
                        if let Some(i) = ui::segmented(ui, "s-motion", &[tr("Full"), tr("Simple"), tr("Off")], s.motion as usize) {
                            s.motion = i as u8;
                        }
                        ui.label(RichText::new([tr("Covers turn like a hardcover; pages curl over the spine."), tr("Pages cross-fade quickly."), tr("Pages change instantly.")][s.motion.min(2) as usize]).font(ui::ui_m(12.0)).color(pal.muted));
                        group(ui, &pal, tr("Turning pages"));
                        ui.checkbox(&mut s.tap_zones, tr("Click the page edges to turn"));
                        ui.checkbox(&mut s.wheel_turns_page, tr("Mouse wheel turns pages"));
                        group(ui, &pal, tr("Auto-advance (A)"));
                        row_slider(ui, &pal, tr("Seconds/page"), &mut s.auto_secs, 5.0..=120.0, 1.0, " s");
                        row_slider(ui, &pal, tr("Scroll speed"), &mut s.auto_px, 5.0..=200.0, 1.0, " px/s");
                        keys_and_dicts(r, ui, &pal, env);
                        group(ui, &pal, tr("Applies to"));
                        let mut pb = per_book as usize;
                        if let Some(i) = ui::segmented(ui, "s-scope", &[tr("All books"), tr("This book")], pb) {
                            pb = i;
                            r.data.settings = if pb == 1 { Some(s.clone()) } else { None };
                            r.dirty = true;
                        }
                        ui.add_space(8.0);
                        if ui::text_button(ui, tr("Reset to defaults"), false).clicked() {
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
    ui.add_space(12.0);
    ui.label(RichText::new(title).font(ui::ui_m(12.0)).color(pal.muted));
}

/// Label | slider | value box: every row uses the same three columns.
fn row_slider(ui: &mut egui::Ui, pal: &Palette, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, suffix: &str) {
    const LABEL_W: f32 = 100.0;
    const VALUE_W: f32 = 76.0;
    let w = ui.available_width();
    let decimals = if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else { 2 };
    ui.allocate_ui_with_layout(vec2(w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(30.0);
        ui.spacing_mut().item_spacing.x = 10.0;
        let (lr, _) = ui.allocate_exact_size(vec2(LABEL_W, 30.0), Sense::hover());
        let text = ui::fit(ui, label, ui::ui_m(13.0), LABEL_W - 4.0);
        ui.painter().text(lr.left_center(), egui::Align2::LEFT_CENTER, text, ui::ui_m(13.0), pal.fg);
        ui.spacing_mut().slider_width = (w - LABEL_W - VALUE_W - 20.0).max(60.0);
        ui.add(egui::Slider::new(v, range.clone()).step_by(step).show_value(false));
        ui.add_sized([VALUE_W, 26.0], egui::DragValue::new(v).range(range).speed(step).fixed_decimals(decimals).suffix(suffix));
    });
}

/// A key drawn as a keycap; hovering shows it can be removed.
fn keycap(ui: &mut egui::Ui, pal: &Palette, text: &str) -> egui::Response {
    let font = ui::mono_m(12.0);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, pal.fg);
    let size = vec2((g.size().x + 16.0).max(28.0), 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hot = resp.hovered();
    let body = rect.with_max_y(rect.bottom() - 2.0);
    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.border);
    ui.painter().rect_filled(body, CornerRadius::same(6), if hot { ui::mix(pal.bg, Color32::from_rgb(220, 80, 60), 0.12) } else { pal.bg });
    ui.painter().rect_stroke(body, CornerRadius::same(6), Stroke::new(1.0, if hot { Color32::from_rgb(220, 80, 60) } else { pal.border }), egui::StrokeKind::Inside);
    ui.painter().galley(body.center() - g.size() / 2.0, g, pal.fg);
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Keyboard shortcut editor and offline dictionary folders (the "More" tab).
fn keys_and_dicts(r: &mut Reader, ui: &mut egui::Ui, pal: &Palette, env: &mut Env) {
    use crate::keys::{self, KeyAction};
    group(ui, pal, tr("Offline dictionaries (StarDict, dictd)"));
    let dirs = env.app.dict_dirs.clone();
    for (i, d) in dirs.iter().enumerate() {
        ui.horizontal(|ui| {
            let n = crate::dict::count_in(d);
            ui.add(egui::Label::new(RichText::new(format!("{d}  ({n})")).font(ui::ui_m(12.0)).color(pal.muted)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Close, tr("Remove dictionary folder")).clicked() {
                    env.app.dict_dirs.remove(i);
                }
            });
        });
    }
    if ui::text_button(ui, tr("Add dictionary folder…"), false).clicked() {
        if let Some(p) = rfd::FileDialog::new().set_title(tr("Folder with StarDict or dictd dictionaries")).pick_folder() {
            let p = p.to_string_lossy().into_owned();
            if !env.app.dict_dirs.contains(&p) {
                env.app.dict_dirs.push(p);
            }
        }
    }
    ui.label(RichText::new(tr("Selected words are looked up here first, then online.")).font(ui::ui_m(12.0)).color(pal.muted));

    group(ui, pal, tr("Keyboard shortcuts"));
    ui.label(RichText::new(tr("Click a key to remove it; + adds another.")).font(ui::ui_m(12.0)).color(pal.muted));
    ui.add_space(6.0);
    let km = keys::Keymap::from_settings(&env.app.keymap);
    const LABEL_W: f32 = 132.0;
    for a in keys::ALL {
        ui::rule(ui);
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            let (lr, _) = ui.allocate_exact_size(vec2(LABEL_W, 26.0), Sense::hover());
            let text = ui::fit(ui, a.label(), ui::ui_m(13.0), LABEL_W - 8.0);
            ui.painter().text(lr.left_center(), egui::Align2::LEFT_CENTER, text, ui::ui_m(13.0), pal.fg);
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for c in km.chords(a) {
                        if keycap(ui, pal, &c.pretty()).on_hover_text(tr("Click to remove this shortcut")).clicked() {
                            let rest: Vec<_> = km.chords(a).iter().copied().filter(|x| x != c).collect();
                            keys::set(&mut env.app.keymap, a, rest);
                            r.key_conflict = None;
                        }
                    }
                    let capturing = r.key_capture == Some(a);
                    let label = if capturing { tr("Press a key… (Esc cancels)") } else { "+" };
                    if ui::text_button(ui, label, capturing).on_hover_text(tr("Add a shortcut")).clicked() {
                        r.key_capture = if capturing { None } else { Some(a) };
                        r.key_conflict = None;
                    }
                });
            });
        });
        ui.add_space(6.0);
    }
    ui::rule(ui);
    ui.add_space(8.0);
    if let Some(w) = &r.key_conflict {
        ui.label(RichText::new(trf!("Warning: {warning}. Both will fire; the first match wins.", warning = w)).font(ui::ui_m(12.0)).color(Color32::from_rgb(220, 120, 60)));
    }
    let _ = KeyAction::Back;
    if ui::text_button(ui, tr("Reset shortcuts to defaults"), false).clicked() {
        env.app.keymap.clear();
        r.key_conflict = None;
        r.key_capture = None;
    }
}
