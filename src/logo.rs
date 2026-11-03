//! The Seshat mark: an ultramarine tile, the goddess's seven-pointed rosette
//! in lime, and an open book beneath it. One geometry, two renderers: egui
//! (live, in the title bar) and a supersampled rasteriser (window icon and the
//! icon files used by installers).

use eframe::egui::{self, pos2, Color32, Pos2};

const TILE: [u8; 3] = [52, 64, 255];
const STAR: [u8; 3] = [214, 246, 92];
const PAGE: [u8; 3] = [248, 247, 243];
const SPINE: [u8; 3] = [28, 27, 34];

/// Shapes in unit space (0..1).
fn star() -> Vec<(f32, f32)> {
    let (cx, cy, ro, ri) = (0.5, 0.37, 0.215, 0.095);
    (0..14)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 7.0;
            let r = if i % 2 == 0 { ro } else { ri };
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn pages() -> [Vec<(f32, f32)>; 2] {
    [
        vec![(0.5, 0.70), (0.2, 0.62), (0.2, 0.80), (0.5, 0.87)],
        vec![(0.5, 0.70), (0.8, 0.62), (0.8, 0.80), (0.5, 0.87)],
    ]
}

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut c = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            c = !c;
        }
        j = i;
    }
    c
}

fn in_tile(x: f32, y: f32) -> bool {
    let r = 0.22;
    let dx = (x - 0.5).abs() - (0.5 - r);
    let dy = (y - 0.5).abs() - (0.5 - r);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    outside + dx.max(dy).min(0.0) <= r
}

/// Render to straight RGBA at `size`×`size` (4×4 supersampling).
pub fn rgba(size: u32) -> Vec<u8> {
    let st = star();
    let pg = pages();
    let mut out = vec![0u8; (size * size * 4) as usize];
    let ss = 4;
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    if !in_tile(x, y) {
                        continue;
                    }
                    let c = if inside(&st, x, y) {
                        STAR
                    } else if (x - 0.5).abs() < 0.012 && y > 0.70 && y < 0.87 {
                        SPINE
                    } else if pg.iter().any(|p| inside(p, x, y)) {
                        PAGE
                    } else if (x - 0.5).abs() < 0.022 && y > 0.5 && y < 0.71 {
                        STAR
                    } else {
                        TILE
                    };
                    acc[0] += c[0] as f32;
                    acc[1] += c[1] as f32;
                    acc[2] += c[2] as f32;
                    acc[3] += 255.0;
                }
            }
            let n = (ss * ss) as f32;
            let a = acc[3] / n;
            let i = ((py * size + px) * 4) as usize;
            if a > 0.0 {
                let k = 255.0 / acc[3];
                out[i] = (acc[0] * k).round() as u8;
                out[i + 1] = (acc[1] * k).round() as u8;
                out[i + 2] = (acc[2] * k).round() as u8;
            }
            out[i + 3] = a.round() as u8;
        }
    }
    out
}

pub fn icon_data(size: u32) -> egui::IconData {
    egui::IconData { rgba: rgba(size), width: size, height: size }
}

/// Paint the mark with egui into `rect`.
pub fn paint(painter: &egui::Painter, rect: egui::Rect) {
    let map = |(x, y): (f32, f32)| -> Pos2 { pos2(rect.left() + x * rect.width(), rect.top() + y * rect.height()) };
    let c = |v: [u8; 3]| Color32::from_rgb(v[0], v[1], v[2]);
    painter.rect_filled(rect, egui::CornerRadius::same((rect.width() * 0.22) as u8), c(TILE));
    let stem = egui::Rect::from_min_max(map((0.478, 0.5)), map((0.522, 0.71)));
    painter.rect_filled(stem, egui::CornerRadius::ZERO, c(STAR));
    for p in pages() {
        painter.add(egui::Shape::convex_polygon(p.into_iter().map(map).collect(), c(PAGE), egui::Stroke::NONE));
    }
    painter.line_segment([map((0.5, 0.70)), map((0.5, 0.87))], egui::Stroke::new((rect.width() * 0.024).max(1.0), c(SPINE)));
    // non-convex star: fan of triangles from the centre
    let st = star();
    let mut m = egui::Mesh::default();
    m.colored_vertex(map((0.5, 0.37)), c(STAR));
    for p in &st {
        m.colored_vertex(map(*p), c(STAR));
    }
    let n = st.len() as u32;
    for i in 0..n {
        m.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(m);
}

/// `seshat --make-icons <dir>`: write the PNG set + Windows ICO for packaging.
pub fn write_icons(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for s in [16u32, 32, 48, 64, 128, 256, 512, 1024] {
        let img = image::RgbaImage::from_raw(s, s, rgba(s)).unwrap();
        img.save(dir.join(format!("seshat-{s}.png"))).map_err(std::io::Error::other)?;
    }
    let img = image::RgbaImage::from_raw(256, 256, rgba(256)).unwrap();
    img.save_with_format(dir.join("seshat.ico"), image::ImageFormat::Ico).map_err(std::io::Error::other)?;
    Ok(())
}
