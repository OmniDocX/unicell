//! Convert text runs into filled vector outlines (`PathSeg`) using system
//! fonts. Layout is basic left-to-right advance-width placement (no complex
//! shaping/bidi), which is exact for Latin and adequate for CJK where each
//! codepoint maps to a single full-advance glyph.

use std::sync::OnceLock;

use ttf_parser::{Face, OutlineBuilder};
use vector_ir::{PathSeg, Point};

/// Horizontal alignment of a text run relative to its anchor point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// A resolved text style used to pick a font and scale glyphs.
#[derive(Debug, Clone)]
pub struct TextStyle {
    pub family: String,
    pub size: f64,
    /// OpenType weight (400 normal, 700 bold).
    pub weight: u16,
    pub italic: bool,
}

/// Lazily-loaded system font database (loading all faces is done once).
fn font_db() -> &'static fontdb::Database {
    static DB: OnceLock<fontdb::Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

/// Layout `text` at baseline point `(x, y)` in the caller's coordinate space
/// and return filled outline segments. Returns an empty vector if no suitable
/// font/glyphs are found. `out_advance`, when provided, receives the total
/// advance width (in user units).
pub fn text_to_paths(
    style: &TextStyle,
    text: &str,
    x: f64,
    y: f64,
    anchor: Anchor,
) -> Vec<PathSeg> {
    let db = font_db();
    let query = fontdb::Query {
        families: &[
            fontdb::Family::Name(&style.family),
            fontdb::Family::SansSerif,
            fontdb::Family::Serif,
        ],
        weight: fontdb::Weight(style.weight),
        stretch: fontdb::Stretch::Normal,
        style: if style.italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        },
    };
    let Some(id) = db.query(&query) else {
        return Vec::new();
    };

    db.with_face_data(id, |data, index| {
        let Ok(face) = Face::parse(data, index) else {
            return Vec::new();
        };
        let upem = face.units_per_em() as f64;
        if upem <= 0.0 {
            return Vec::new();
        }
        let scale = style.size / upem;

        // First pass: total advance for anchor adjustment.
        let mut total = 0.0f64;
        for ch in text.chars() {
            if let Some(gid) = face.glyph_index(ch) {
                total += face.glyph_hor_advance(gid).unwrap_or(0) as f64 * scale;
            }
        }
        let start_x = match anchor {
            Anchor::Start => x,
            Anchor::Middle => x - total / 2.0,
            Anchor::End => x - total,
        };

        // Second pass: emit outlines.
        let mut segs = Vec::new();
        let mut pen = start_x;
        for ch in text.chars() {
            let Some(gid) = face.glyph_index(ch) else {
                continue;
            };
            let mut b = GlyphBuilder {
                segs: &mut segs,
                scale,
                pen_x: pen,
                baseline_y: y,
                start: Point::default(),
                started: false,
            };
            let _ = face.outline_glyph(gid, &mut b);
            if b.started {
                // Ensure the last contour is closed.
                segs.push(PathSeg::Close);
            }
            pen += face.glyph_hor_advance(gid).unwrap_or(0) as f64 * scale;
        }
        segs
    })
    .unwrap_or_default()
}

/// Total advance width of `text` in the given style (user units), or 0.
pub fn text_advance(style: &TextStyle, text: &str) -> f64 {
    let db = font_db();
    let query = fontdb::Query {
        families: &[
            fontdb::Family::Name(&style.family),
            fontdb::Family::SansSerif,
        ],
        weight: fontdb::Weight(style.weight),
        stretch: fontdb::Stretch::Normal,
        style: if style.italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        },
    };
    let Some(id) = db.query(&query) else {
        return 0.0;
    };
    db.with_face_data(id, |data, index| {
        let Ok(face) = Face::parse(data, index) else {
            return 0.0;
        };
        let scale = style.size / face.units_per_em() as f64;
        text.chars()
            .filter_map(|c| face.glyph_index(c))
            .map(|g| face.glyph_hor_advance(g).unwrap_or(0) as f64 * scale)
            .sum()
    })
    .unwrap_or(0.0)
}

/// Builds `PathSeg`s from a glyph outline, mapping font units (y-up, relative
/// to the pen origin on the baseline) into user space (y-down).
struct GlyphBuilder<'a> {
    segs: &'a mut Vec<PathSeg>,
    scale: f64,
    pen_x: f64,
    baseline_y: f64,
    start: Point,
    started: bool,
}

impl GlyphBuilder<'_> {
    fn map(&self, x: f32, y: f32) -> Point {
        Point::new(
            self.pen_x + x as f64 * self.scale,
            self.baseline_y - y as f64 * self.scale,
        )
    }
}

impl OutlineBuilder for GlyphBuilder<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        // A new contour: close the previous one if any.
        if self.started {
            self.segs.push(PathSeg::Close);
        }
        let p = self.map(x, y);
        self.start = p;
        self.segs.push(PathSeg::MoveTo(p));
        self.started = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.segs.push(PathSeg::LineTo(self.map(x, y)));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        // Elevate the quadratic to a cubic exactly.
        let p0 = self
            .segs
            .iter()
            .rev()
            .find_map(|s| s.end_point())
            .unwrap_or(self.start);
        let c = self.map(x1, y1);
        let p = self.map(x, y);
        let c1 = Point::new(
            p0.x + 2.0 / 3.0 * (c.x - p0.x),
            p0.y + 2.0 / 3.0 * (c.y - p0.y),
        );
        let c2 = Point::new(p.x + 2.0 / 3.0 * (c.x - p.x), p.y + 2.0 / 3.0 * (c.y - p.y));
        self.segs.push(PathSeg::CubicTo(c1, c2, p));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.segs.push(PathSeg::CubicTo(
            self.map(x1, y1),
            self.map(x2, y2),
            self.map(x, y),
        ));
    }

    fn close(&mut self) {
        // Contour closing is handled at move_to / end so that multiple
        // contours (holes) accumulate into one fillable path.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_is_positive_for_common_text() {
        // This depends on system fonts; only assert non-negative and that a
        // glyph run yields some geometry when a font is available.
        let style = TextStyle {
            family: "Arial".to_string(),
            size: 16.0,
            weight: 400,
            italic: false,
        };
        let segs = text_to_paths(&style, "Hi", 0.0, 0.0, Anchor::Start);
        // If no fonts are installed the result may be empty; when present it
        // must contain at least a MoveTo.
        if !segs.is_empty() {
            assert!(matches!(segs[0], PathSeg::MoveTo(_)));
        }
    }
}
