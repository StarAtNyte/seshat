//! Visual design system: reader palettes, egui styling, painted icons.

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};

use crate::store::{ReaderSettings, Theme};

pub const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub fg: Color32,
    pub muted: Color32,
    pub link: Color32,
    pub panel: Color32,
    pub border: Color32,
    pub hover: Color32,
    pub dark: bool,
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn palette_for(theme: Theme, custom_bg: [u8; 3], custom_fg: [u8; 3]) -> Palette {
    let (bg, fg, link) = match theme {
        Theme::Light => (rgb([255, 255, 255]), rgb([31, 35, 40]), rgb([37, 99, 235])),
        Theme::Paper => (rgb([251, 248, 241]), rgb([43, 42, 39]), rgb([30, 90, 200])),
        Theme::Sepia => (rgb([244, 236, 216]), rgb([91, 70, 54]), rgb([150, 80, 30])),
        Theme::Gray => (rgb([218, 218, 214]), rgb([34, 34, 34]), rgb([30, 80, 170])),
        Theme::Dark => (rgb([30, 31, 34]), rgb([212, 212, 212]), rgb([120, 170, 255])),
        Theme::Nord => (rgb([46, 52, 64]), rgb([216, 222, 233]), rgb([136, 192, 208])),
        Theme::Black => (rgb([0, 0, 0]), rgb([190, 190, 190]), rgb([110, 160, 240])),
        Theme::Custom => {
            let bg = rgb(custom_bg);
            let fg = rgb(custom_fg);
            (bg, fg, mix(fg, ACCENT, 0.6))
        }
    };
    let dark = luminance(bg) < 0.45;
    Palette {
        bg,
        fg,
        muted: mix(bg, fg, 0.55),
        link,
        panel: if dark { mix(bg, Color32::WHITE, 0.06) } else { mix(bg, Color32::BLACK, 0.035) },
        border: mix(bg, fg, 0.14),
        hover: mix(bg, fg, 0.08),
        dark,
    }
}

pub fn palette(s: &ReaderSettings) -> Palette {
    palette_for(s.theme, s.custom_bg, s.custom_fg)
}

pub fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

/// App-wide style tuned for a clean, modern look.
pub fn style(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.extreme_bg_color = if p.dark { mix(p.bg, Color32::BLACK, 0.3) } else { mix(p.bg, Color32::WHITE, 0.6) };
    v.faint_bg_color = p.hover;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if p.dark { 100 } else { 35 }) };
    v.selection.bg_fill = ACCENT.linear_multiply(if p.dark { 0.55 } else { 0.25 });
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = p.link;
    v.override_text_color = Some(p.fg);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.fg);
    v.widgets.inactive.weak_bg_fill = p.hover;
    v.widgets.inactive.bg_fill = p.hover;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.fg);
    v.widgets.hovered.weak_bg_fill = mix(p.hover, p.fg, 0.08);
    v.widgets.hovered.bg_fill = mix(p.hover, p.fg, 0.08);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, p.fg);
    v.widgets.active.weak_bg_fill = mix(p.hover, p.fg, 0.15);
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.fg_stroke = Stroke::new(1.5, p.fg);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.slider_width = 170.0;
        s.spacing.menu_margin = egui::Margin::same(8);
        s.spacing.window_margin = egui::Margin::same(14);
        use egui::{FontFamily::Proportional, FontId, TextStyle::*};
        s.text_styles = [
            (Heading, FontId::new(21.0, Proportional)),
            (Body, FontId::new(14.5, Proportional)),
            (Button, FontId::new(14.0, Proportional)),
            (Small, FontId::new(12.0, Proportional)),
            (Monospace, FontId::new(13.0, egui::FontFamily::Monospace)),
        ]
        .into();
    });
}

// ---------- icons ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Back,
    Menu,
    Search,
    Bookmark,
    BookmarkOn,
    Font,
    Speaker,
    Pause,
    Play,
    Stop,
    Fullscreen,
    Close,
    Left,
    Right,
    Grid,
    List,
    Plus,
    Gear,
    Folder,
    Globe,
    Library,
    Note,
    Pen,
    Trash,
    Copy,
    Book,
    Translate,
    Undo,
    Sort,
    Info,
    Download,
    Refresh,
    ZoomIn,
    ZoomOut,
    Sync,
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) / 20.0; // design grid: 20 units
    let st = Stroke::new(1.6 * s.max(0.8), color);
    let p = |x: f32, y: f32| pos2(c.x + (x - 10.0) * s, c.y + (y - 10.0) * s);
    let line = |a: Pos2, b: Pos2| painter.line_segment([a, b], st);
    let poly = |pts: Vec<Pos2>, closed: bool| {
        if closed {
            painter.add(Shape::closed_line(pts, st));
        } else {
            painter.add(Shape::line(pts, st));
        }
    };
    match icon {
        Icon::Back => poly(vec![p(12.5, 4.0), p(6.5, 10.0), p(12.5, 16.0)], false),
        Icon::Left => poly(vec![p(12.0, 5.0), p(7.0, 10.0), p(12.0, 15.0)], false),
        Icon::Right => poly(vec![p(8.0, 5.0), p(13.0, 10.0), p(8.0, 15.0)], false),
        Icon::Menu => {
            for y in [5.5, 10.0, 14.5] {
                line(p(3.5, y), p(16.5, y));
            }
        }
        Icon::List => {
            for y in [5.5, 10.0, 14.5] {
                painter.circle_filled(p(4.0, y), 1.1 * s, color);
                line(p(7.0, y), p(16.5, y));
            }
        }
        Icon::Grid => {
            for (x, y) in [(3.5, 3.5), (11.0, 3.5), (3.5, 11.0), (11.0, 11.0)] {
                painter.rect_stroke(Rect::from_min_max(p(x, y), p(x + 5.5, y + 5.5)), CornerRadius::same((1.5 * s) as u8), st, egui::StrokeKind::Middle);
            }
        }
        Icon::Search => {
            painter.circle_stroke(p(8.5, 8.5), 5.0 * s, st);
            line(p(12.3, 12.3), p(16.5, 16.5));
        }
        Icon::Bookmark | Icon::BookmarkOn => {
            let pts = vec![p(5.5, 3.0), p(14.5, 3.0), p(14.5, 17.0), p(10.0, 13.0), p(5.5, 17.0)];
            if icon == Icon::BookmarkOn {
                painter.add(Shape::convex_polygon(pts.clone(), color, Stroke::NONE));
            }
            poly(pts, true);
        }
        Icon::Font => {
            painter.text(p(7.0, 11.0), egui::Align2::CENTER_CENTER, "A", egui::FontId::proportional(15.0 * s), color);
            painter.text(p(14.5, 12.5), egui::Align2::CENTER_CENTER, "a", egui::FontId::proportional(11.0 * s), color);
        }
        Icon::Speaker => {
            poly(vec![p(3.0, 7.5), p(6.0, 7.5), p(10.0, 4.0), p(10.0, 16.0), p(6.0, 12.5), p(3.0, 12.5)], true);
            let arc = |r: f32| {
                let pts: Vec<Pos2> = (0..=8).map(|i| {
                    let a = -0.8 + 1.6 * i as f32 / 8.0;
                    p(10.0 + r * a.cos(), 10.0 + r * a.sin())
                }).collect();
                poly(pts, false);
            };
            arc(3.5);
            arc(6.5);
        }
        Icon::Pause => {
            line(p(7.0, 4.5), p(7.0, 15.5));
            line(p(13.0, 4.5), p(13.0, 15.5));
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(vec![p(6.5, 4.0), p(15.5, 10.0), p(6.5, 16.0)], color, Stroke::NONE));
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_min_max(p(5.5, 5.5), p(14.5, 14.5)), CornerRadius::same((1.5 * s) as u8), color);
        }
        Icon::Fullscreen => {
            for (a, b, cc) in [((3.5, 7.5), (3.5, 3.5), (7.5, 3.5)), ((12.5, 3.5), (16.5, 3.5), (16.5, 7.5)), ((16.5, 12.5), (16.5, 16.5), (12.5, 16.5)), ((7.5, 16.5), (3.5, 16.5), (3.5, 12.5))] {
                poly(vec![p(a.0, a.1), p(b.0, b.1), p(cc.0, cc.1)], false);
            }
        }
        Icon::Close => {
            line(p(5.0, 5.0), p(15.0, 15.0));
            line(p(15.0, 5.0), p(5.0, 15.0));
        }
        Icon::Plus => {
            line(p(10.0, 4.0), p(10.0, 16.0));
            line(p(4.0, 10.0), p(16.0, 10.0));
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            painter.circle_stroke(p(8.5, 8.5), 5.0 * s, st);
            line(p(12.3, 12.3), p(16.5, 16.5));
            line(p(6.0, 8.5), p(11.0, 8.5));
            if icon == Icon::ZoomIn {
                line(p(8.5, 6.0), p(8.5, 11.0));
            }
        }
        Icon::Gear => {
            painter.circle_stroke(p(10.0, 10.0), 2.6 * s, st);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                line(p(10.0 + 5.0 * a.cos(), 10.0 + 5.0 * a.sin()), p(10.0 + 7.2 * a.cos(), 10.0 + 7.2 * a.sin()));
            }
            painter.circle_stroke(p(10.0, 10.0), 5.0 * s, st);
        }
        Icon::Folder => poly(vec![p(2.5, 5.0), p(8.0, 5.0), p(9.5, 6.8), p(17.5, 6.8), p(17.5, 15.5), p(2.5, 15.5)], true),
        Icon::Globe => {
            painter.circle_stroke(p(10.0, 10.0), 7.0 * s, st);
            line(p(3.0, 10.0), p(17.0, 10.0));
            let pts: Vec<Pos2> = (0..=12).map(|i| {
                let t = -1.0 + 2.0 * i as f32 / 12.0;
                p(10.0 + 3.2 * (1.0 - t * t).sqrt(), 10.0 + 7.0 * t)
            }).collect();
            poly(pts.clone(), false);
            poly(pts.iter().map(|q| pos2(2.0 * c.x - q.x, q.y)).collect(), false);
        }
        Icon::Library | Icon::Book => {
            poly(vec![p(10.0, 5.0), p(3.0, 3.5), p(3.0, 15.5), p(10.0, 17.0), p(17.0, 15.5), p(17.0, 3.5)], true);
            line(p(10.0, 5.0), p(10.0, 17.0));
        }
        Icon::Note => {
            poly(vec![p(4.0, 3.5), p(16.0, 3.5), p(16.0, 12.5), p(11.5, 16.5), p(4.0, 16.5)], true);
            line(p(7.0, 7.5), p(13.0, 7.5));
            line(p(7.0, 10.5), p(11.0, 10.5));
        }
        Icon::Pen => {
            poly(vec![p(4.0, 16.0), p(4.8, 12.5), p(13.0, 4.3), p(15.7, 7.0), p(7.5, 15.2)], true);
            line(p(11.5, 5.8), p(14.2, 8.5));
        }
        Icon::Trash => {
            line(p(3.5, 5.5), p(16.5, 5.5));
            poly(vec![p(5.0, 5.5), p(6.0, 16.5), p(14.0, 16.5), p(15.0, 5.5)], false);
            poly(vec![p(8.0, 5.5), p(8.0, 3.5), p(12.0, 3.5), p(12.0, 5.5)], false);
        }
        Icon::Copy => {
            painter.rect_stroke(Rect::from_min_max(p(7.0, 7.0), p(16.5, 16.5)), CornerRadius::same((1.5 * s) as u8), st, egui::StrokeKind::Middle);
            poly(vec![p(4.5, 13.0), p(3.5, 13.0), p(3.5, 3.5), p(13.0, 3.5), p(13.0, 4.5)], false);
        }
        Icon::Translate => {
            painter.text(p(6.5, 8.0), egui::Align2::CENTER_CENTER, "A", egui::FontId::proportional(10.0 * s), color);
            painter.text(p(13.5, 13.0), egui::Align2::CENTER_CENTER, "文", egui::FontId::proportional(9.5 * s), color);
            line(p(11.0, 4.5), p(15.0, 4.5));
        }
        Icon::Undo => {
            poly(vec![p(7.0, 4.0), p(3.5, 7.5), p(7.0, 11.0)], false);
            let pts: Vec<Pos2> = (0..=10).map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 10.0;
                p(11.0 + 5.0 * a.cos(), 12.5 + 5.0 * a.sin())
            }).collect();
            line(p(3.5, 7.5), p(11.0, 7.5));
            poly(pts, false);
        }
        Icon::Sort => {
            poly(vec![p(6.0, 15.5), p(6.0, 4.0)], false);
            poly(vec![p(3.5, 6.5), p(6.0, 4.0), p(8.5, 6.5)], false);
            poly(vec![p(14.0, 4.5), p(14.0, 16.0)], false);
            poly(vec![p(11.5, 13.5), p(14.0, 16.0), p(16.5, 13.5)], false);
        }
        Icon::Info => {
            painter.circle_stroke(p(10.0, 10.0), 7.0 * s, st);
            line(p(10.0, 9.0), p(10.0, 14.0));
            painter.circle_filled(p(10.0, 6.3), 1.0 * s, color);
        }
        Icon::Download => {
            line(p(10.0, 3.5), p(10.0, 12.5));
            poly(vec![p(6.0, 8.5), p(10.0, 12.5), p(14.0, 8.5)], false);
            line(p(4.0, 16.0), p(16.0, 16.0));
        }
        Icon::Refresh | Icon::Sync => {
            let pts: Vec<Pos2> = (0..=14).map(|i| {
                let a = 0.3 + 5.0 * i as f32 / 14.0;
                p(10.0 + 6.0 * a.cos(), 10.0 + 6.0 * a.sin())
            }).collect();
            let last = *pts.last().unwrap();
            poly(pts, false);
            painter.add(Shape::convex_polygon(vec![last + vec2(-2.5 * s, -1.0 * s), last + vec2(1.5 * s, -2.8 * s), last + vec2(1.2 * s, 1.8 * s)], color, Stroke::NONE));
        }
    }
}

pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    icon_toggle(ui, icon, tip, false)
}

pub fn icon_toggle(ui: &mut Ui, icon: Icon, tip: &str, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let vis = ui.visuals();
    let fg = vis.override_text_color.unwrap_or(vis.text_color());
    if active {
        ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(8), ACCENT.linear_multiply(0.18));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(8), vis.widgets.hovered.weak_bg_fill);
    }
    let color = if active { ACCENT } else { fg.linear_multiply(if resp.hovered() { 1.0 } else { 0.85 }) };
    paint_icon(ui.painter(), rect.shrink(7.0), icon, color);
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// Primary call-to-action button.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong()).fill(ACCENT).corner_radius(CornerRadius::same(8)).min_size(vec2(0.0, 30.0)))
}

/// Pill-style toggle chip.
pub fn chip(ui: &mut Ui, selected: bool, text: &str) -> Response {
    let fg = ui.visuals().override_text_color.unwrap_or(ui.visuals().text_color());
    let b = egui::Button::new(egui::RichText::new(text).color(if selected { Color32::WHITE } else { fg }).size(13.0))
        .fill(if selected { ACCENT } else { ui.visuals().widgets.inactive.weak_bg_fill })
        .corner_radius(CornerRadius::same(14))
        .min_size(vec2(0.0, 26.0));
    ui.add(b)
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text.to_uppercase()).size(11.0).strong().color(ui.visuals().weak_text_color()));
}

/// Generated cover for books without one.
pub fn paint_placeholder_cover(painter: &egui::Painter, rect: Rect, title: &str, author: &str) {
    let h = title.bytes().fold(5381u32, |a, b| a.wrapping_mul(33) ^ b as u32);
    let hues = [[67, 97, 238], [214, 92, 92], [46, 160, 120], [180, 120, 50], [130, 80, 200], [40, 140, 170], [200, 80, 140], [90, 110, 130]];
    let base = hues[(h % hues.len() as u32) as usize];
    let c1 = Color32::from_rgb(base[0], base[1], base[2]);
    let c2 = mix(c1, Color32::BLACK, 0.35);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), c1);
    mesh.colored_vertex(rect.right_top(), c1);
    mesh.colored_vertex(rect.right_bottom(), c2);
    mesh.colored_vertex(rect.left_bottom(), c2);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
    painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(rect.width() * 0.06, 0.0), vec2(2.0, rect.height())), CornerRadius::ZERO, Color32::from_white_alpha(40));
    let w = rect.width() * 0.8;
    let font = egui::FontId::new((rect.width() / 9.0).clamp(10.0, 20.0), egui::FontFamily::Name("book-b".into()));
    let job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping { max_width: w, max_rows: 5, ..Default::default() },
        halign: egui::Align::Center,
        ..egui::text::LayoutJob::single_section(title.to_string(), egui::TextFormat::simple(font, Color32::WHITE))
    };
    let g = painter.layout_job(job);
    painter.galley(pos2(rect.center().x, rect.top() + rect.height() * 0.18), g, Color32::WHITE);
    if !author.is_empty() {
        let font = egui::FontId::proportional((rect.width() / 13.0).clamp(9.0, 14.0));
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping { max_width: w, max_rows: 2, ..Default::default() },
            halign: egui::Align::Center,
            ..egui::text::LayoutJob::single_section(author.to_string(), egui::TextFormat::simple(font, Color32::from_white_alpha(210)))
        };
        let g = painter.layout_job(job);
        painter.galley(pos2(rect.center().x, rect.bottom() - rect.height() * 0.12 - g.size().y), g, Color32::WHITE);
    }
}
