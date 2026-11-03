//! Small painted widgets shared by the library views (star ratings).

use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Sense, Shape, Stroke, Ui};

fn star_points(c: Pos2, r: f32) -> [Pos2; 10] {
    let mut p = [Pos2::ZERO; 10];
    for (i, q) in p.iter_mut().enumerate() {
        let ang = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let rad = if i % 2 == 0 { r } else { r * 0.44 };
        *q = c + vec2(ang.cos(), ang.sin()) * rad;
    }
    p
}

/// A five-point star: filled (as ten triangles, so the concave shape fills
/// correctly) or just an outline.
pub fn paint_star(painter: &Painter, c: Pos2, r: f32, filled: bool, col: Color32) {
    let p = star_points(c, r);
    if filled {
        for i in 0..10 {
            painter.add(Shape::convex_polygon(vec![c, p[i], p[(i + 1) % 10]], col, Stroke::new(0.6, col)));
        }
    } else {
        painter.add(Shape::closed_line(p.to_vec(), Stroke::new(1.0, col)));
    }
}

/// Interactive 0-5 star row. Clicking the current rating clears it.
pub fn star_row(ui: &mut Ui, rating: u8, size: f32, on: Color32, off: Color32) -> Option<u8> {
    let pitch = size * 1.25;
    let (rect, _) = ui.allocate_exact_size(vec2(pitch * 5.0, size * 1.3), Sense::hover());
    let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)).map(|p| (((p.x - rect.left()) / pitch).floor() as i32 + 1).clamp(1, 5) as u8);
    let mut out = None;
    for n in 1..=5u8 {
        let c = pos2(rect.left() + pitch * (n as f32 - 0.5), rect.center().y);
        let lit = hover.map(|h| n <= h).unwrap_or(n <= rating);
        paint_star(ui.painter(), c, size * 0.5, lit, if lit { on } else { off });
    }
    let resp = ui.interact(rect, ui.id().with(("stars", rect.min.x as i32, rect.min.y as i32)), Sense::click());
    if resp.clicked() {
        if let Some(h) = hover {
            out = Some(if h == rating { 0 } else { h });
        }
    }
    if hover.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    out
}
