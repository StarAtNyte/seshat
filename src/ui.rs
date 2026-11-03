//! Visual identity: soft paper, deep ink, an ultramarine accent with a lime
//! highlighter, Bricolage Grotesque throughout, thin rules, playful details
//! (pill toggles, wavy hover underlines, geometric placeholder covers).

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, Ui};

use crate::store::{ReaderSettings, Theme};

/// Ultramarine accent.
pub const ACCENT: Color32 = Color32::from_rgb(52, 64, 255);
/// Lime highlighter, used for selection and "you are here" marks.
pub const LIME: Color32 = Color32::from_rgb(214, 246, 92);

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

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub const PAPER: Color32 = Color32::from_rgb(248, 247, 243);
pub const INK: Color32 = Color32::from_rgb(28, 27, 34);

pub fn palette_for(theme: Theme, custom_bg: [u8; 3], custom_fg: [u8; 3]) -> Palette {
    let (bg, fg, link) = match theme {
        Theme::Light => (rgb([255, 255, 255]), rgb([28, 27, 34]), rgb([52, 64, 230])),
        Theme::Paper => (PAPER, INK, rgb([52, 64, 230])),
        Theme::Sepia => (rgb([241, 231, 210]), rgb([78, 60, 44]), rgb([150, 70, 30])),
        Theme::Gray => (rgb([222, 222, 218]), rgb([30, 30, 34]), rgb([52, 64, 210])),
        Theme::Dark => (rgb([24, 24, 28]), rgb([224, 222, 230]), rgb([140, 150, 255])),
        Theme::Nord => (rgb([36, 41, 52]), rgb([216, 222, 233]), rgb([136, 192, 208])),
        Theme::Black => (rgb([0, 0, 0]), rgb([196, 194, 200]), rgb([140, 150, 255])),
        Theme::Custom => {
            let bg = rgb(custom_bg);
            let fg = rgb(custom_fg);
            (bg, fg, mix(fg, ACCENT, 0.7))
        }
    };
    let dark = luminance(bg) < 0.45;
    Palette {
        bg,
        fg,
        muted: mix(bg, fg, 0.55),
        link,
        panel: bg,
        border: mix(bg, fg, 0.18),
        hover: mix(bg, fg, 0.06),
        dark,
    }
}

pub fn palette(s: &ReaderSettings) -> Palette {
    palette_for(s.theme, s.custom_bg, s.custom_fg)
}

pub fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

pub fn ui_m(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("ui-m".into()))
}
pub fn ui_b(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("ui-b".into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("label".into()))
}
pub fn mono_m(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("label-m".into()))
}

/// Small quiet label in the UI face.
pub fn tag(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).font(ui_m(12.0))
}

/// Display headline in bold grotesk.
pub fn display(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).font(ui_b(size)).extra_letter_spacing(-size * 0.02)
}

pub fn style(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let sharp = CornerRadius::ZERO;
    v.panel_fill = p.bg;
    v.window_fill = p.bg;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.hover;
    v.code_bg_color = p.hover;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.selection.bg_fill = if p.dark { ACCENT.linear_multiply(0.5) } else { LIME };
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = p.link;
    v.override_text_color = Some(p.fg);
    v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    let _ = sharp;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(6);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.fg);
    v.widgets.noninteractive.bg_fill = p.bg;
    v.widgets.noninteractive.weak_bg_fill = p.bg;
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = p.border;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.fg);
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.fg;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.fg);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, p.fg);
    v.widgets.active.weak_bg_fill = p.fg;
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.fg);
    v.widgets.active.fg_stroke = Stroke::new(1.5, p.bg);
    v.widgets.open.weak_bg_fill = p.hover;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(10.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.slider_width = 170.0;
        s.spacing.slider_rail_height = 2.0;
        s.spacing.menu_margin = egui::Margin::same(10);
        s.spacing.window_margin = egui::Margin::same(18);
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.interaction.selectable_labels = false;
        use egui::{FontFamily::Proportional, TextStyle::*};
        s.text_styles = [
            (Heading, ui_b(22.0)),
            (Body, FontId::new(14.5, Proportional)),
            (Button, FontId::new(14.0, Proportional)),
            (Small, FontId::new(12.0, Proportional)),
            (Monospace, mono(12.5)),
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
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let fg = ui.visuals().override_text_color.unwrap_or(ui.visuals().text_color());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::ZERO, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let color = if active { ACCENT } else { fg };
    paint_icon(ui.painter(), rect.shrink(7.5), icon, color);
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// Text toggle. Active: soft accent pill. Hover: a little wavy underline.
pub fn text_button(ui: &mut Ui, text: &str, active: bool) -> Response {
    let fg = ui.visuals().override_text_color.unwrap_or(ui.visuals().text_color());
    let g = ui.painter().layout_no_wrap(text.to_string(), ui_m(13.5), fg);
    let size = vec2(g.size().x + 18.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.14);
    let a = ui.ctx().animate_bool_with_time(resp.id.with("a"), active, 0.18);
    if a > 0.01 {
        let pill = Rect::from_center_size(rect.center(), vec2(rect.width() * (0.7 + 0.3 * a), rect.height() - 2.0));
        ui.painter().rect_filled(pill, CornerRadius::same(14), ACCENT.linear_multiply(0.13 * a));
    }
    let color = if active { ACCENT } else { fg.linear_multiply(0.75 + 0.25 * t) };
    let x = rect.left() + 9.0;
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, color);
    if t > 0.01 && !active {
        let w = (rect.width() - 18.0) * t;
        let y = rect.bottom() - 4.0;
        let pts: Vec<Pos2> = (0..=(w / 2.0) as usize).map(|i| {
            let xx = i as f32 * 2.0;
            pos2(x + xx, y + (xx * 0.6).sin() * 1.3)
        }).collect();
        if pts.len() > 1 {
            ui.painter().add(Shape::line(pts, Stroke::new(1.2, ACCENT)));
        }
    }
    resp
}

/// Solid accent button with rounded ends.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), ui_m(14.0), Color32::WHITE);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 32.0, 36.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
    let fill = mix(ACCENT, INK, 0.35 * t);
    ui.painter().rect_filled(rect, CornerRadius::same(18), fill);
    ui.painter().galley(rect.center() - g.size() / 2.0 + vec2(-3.0 * t, 0.0), g, Color32::WHITE);
    if t > 0.01 {
        ui.painter().text(rect.right_center() - vec2(14.0 - 4.0 * t, 0.0), egui::Align2::RIGHT_CENTER, "→", ui_m(14.0), Color32::WHITE.linear_multiply(t));
    }
    resp
}

/// Navigation / filter item: text with optional count.
pub fn chip(ui: &mut Ui, selected: bool, text: &str) -> Response {
    text_button(ui, text, selected)
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    let c = ui.visuals().weak_text_color();
    ui.label(tag(text).color(c));
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
    ui.add_space(4.0);
}

pub fn rule(ui: &mut Ui) {
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
    ui.add_space(1.0);
}

const COVER_TINTS: &[([u8; 3], [u8; 3])] = &[
    ([214, 246, 92], [28, 27, 34]),
    ([52, 64, 255], [248, 247, 243]),
    ([255, 196, 214], [60, 20, 40]),
    ([255, 214, 102], [50, 36, 10]),
    ([180, 226, 214], [20, 60, 50]),
    ([28, 27, 34], [214, 246, 92]),
    ([255, 128, 90], [40, 16, 10]),
    ([205, 196, 255], [36, 24, 90]),
];

/// Playful geometric cover for books without one: a tint, one shape, the title.
pub fn paint_placeholder_cover(painter: &egui::Painter, rect: Rect, title: &str, author: &str) {
    let h = title.bytes().fold(5381u32, |a, b| a.wrapping_mul(33) ^ b as u32);
    let (bgc, fgc) = COVER_TINTS[(h % COVER_TINTS.len() as u32) as usize];
    let bg = Color32::from_rgb(bgc[0], bgc[1], bgc[2]);
    let fg = Color32::from_rgb(fgc[0], fgc[1], fgc[2]);
    painter.rect_filled(rect, CornerRadius::same(3), bg);
    let w = rect.width();
    let shape_c = fg.linear_multiply(0.16);
    match (h >> 8) % 4 {
        0 => {
            painter.circle_filled(pos2(rect.right() - w * 0.18, rect.top() + w * 0.3), w * 0.42, shape_c);
        }
        1 => {
            for i in 0..5 {
                let y = rect.top() + rect.height() * (0.08 + i as f32 * 0.05);
                painter.hline(rect.left() + w * 0.08..=rect.right() - w * 0.08, y, Stroke::new((w * 0.012).max(1.0), shape_c));
            }
        }
        2 => {
            let c = pos2(rect.center().x, rect.top() + rect.height() * 0.38);
            let pts: Vec<Pos2> = (0..=24).map(|i| {
                let a = std::f32::consts::PI * i as f32 / 24.0;
                pos2(c.x - w * 0.42 * a.cos(), c.y - w * 0.42 * a.sin())
            }).collect();
            painter.add(Shape::convex_polygon(pts, shape_c, Stroke::NONE));
        }
        _ => {
            let s = w * 0.3;
            painter.rect_filled(Rect::from_min_size(pos2(rect.left() + w * 0.1, rect.top() + w * 0.12), vec2(s, s)), CornerRadius::ZERO, shape_c);
            painter.circle_filled(pos2(rect.left() + w * 0.1 + s * 1.5, rect.top() + w * 0.12 + s * 0.5), s * 0.5, shape_c);
        }
    }
    let pad = (w * 0.09).max(4.0);
    let size = (w / 8.0).clamp(9.0, 26.0);
    let mut job = egui::text::LayoutJob::single_section(title.to_string(), egui::TextFormat { font_id: ui_b(size), color: fg, line_height: Some(size * 1.05), ..Default::default() });
    job.wrap = egui::text::TextWrapping { max_width: w - 2.0 * pad, max_rows: 5, overflow_character: Some('…'), ..Default::default() };
    let g = painter.layout_job(job);
    let au_h = if author.is_empty() || w < 50.0 { 0.0 } else { size * 0.9 };
    painter.galley(pos2(rect.left() + pad, rect.bottom() - pad - g.size().y - au_h), g, fg);
    if au_h > 0.0 {
        let mut job = egui::text::LayoutJob::single_section(author.to_string(), egui::TextFormat::simple(ui_m((w / 15.0).clamp(7.5, 12.0)), fg.linear_multiply(0.8)));
        job.wrap = egui::text::TextWrapping { max_width: w - 2.0 * pad, max_rows: 1, overflow_character: Some('…'), ..Default::default() };
        let g = painter.layout_job(job);
        painter.galley(pos2(rect.left() + pad, rect.bottom() - pad - g.size().y), g, fg);
    }
}

#[allow(dead_code)]
fn _keep(_: Pos2, _: Shape) {}
