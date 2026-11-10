//! The microphone glyph both tray icons are drawn from: Windows colours it, macOS uses it
//! as a template image the menu bar tints.

/// Edge length of the glyph in pixels.
pub(super) const GLYPH_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GlyphPart {
    /// The rounded microphone body.
    Capsule,
    /// The stem and base under it.
    Stand,
}

/// The part of the glyph covering pixel (`x`, `y`), counted from the top-left corner.
pub(super) fn glyph_part(x: usize, y: usize) -> Option<GlyphPart> {
    if in_rounded_rect(x, y, (11, 3, 20, 19), 4.5) {
        Some(GlyphPart::Capsule)
    } else if ((15..=16).contains(&x) && (19..=24).contains(&y))
        || in_rounded_rect(x, y, (9, 25, 22, 27), 2.0)
    {
        Some(GlyphPart::Stand)
    } else {
        None
    }
}

/// Whether (`x`, `y`) lies inside the inclusive rectangle with corners rounded to `radius`.
fn in_rounded_rect(
    x: usize,
    y: usize,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
    radius: f32,
) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let left = x0 as f32 + radius;
    let right = x1 as f32 - radius;
    let top = y0 as f32 + radius;
    let bottom = y1 as f32 - radius;
    let dx = (left - x as f32).max(0.0).max(x as f32 - right);
    let dy = (top - y as f32).max(0.0).max(y as f32 - bottom);
    dx * dx + dy * dy <= radius * radius + 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_covers_the_upper_middle() {
        assert_eq!(glyph_part(15, 10), Some(GlyphPart::Capsule));
    }

    #[test]
    fn stem_and_base_form_the_stand() {
        assert_eq!(glyph_part(15, 22), Some(GlyphPart::Stand));
        assert_eq!(glyph_part(12, 26), Some(GlyphPart::Stand));
    }

    #[test]
    fn rounded_corners_and_margins_stay_transparent() {
        assert_eq!(glyph_part(0, 0), None);
        assert_eq!(glyph_part(11, 3), None);
        assert_eq!(glyph_part(GLYPH_SIZE - 1, GLYPH_SIZE - 1), None);
    }
}
