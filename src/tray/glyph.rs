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
    /// The diagonal stroke across a muted microphone.
    Slash,
}

/// Ends of the mute slash, from top right to bottom left.
const SLASH_START: (f32, f32) = (27.0, 4.0);
const SLASH_END: (f32, f32) = (5.0, 28.0);
/// Half the slash's thickness, and how far from its centre line the glyph is cleared so the
/// slash stands apart from the microphone behind it.
const SLASH_HALF_WIDTH: f32 = 1.4;
const SLASH_CLEARANCE: f32 = 3.2;

/// The part of the muted glyph covering pixel (`x`, `y`): the live glyph with a slash
/// through it and a transparent margin either side of the slash.
pub(super) fn muted_glyph_part(x: usize, y: usize) -> Option<GlyphPart> {
    let (dx, dy) = (SLASH_END.0 - SLASH_START.0, SLASH_END.1 - SLASH_START.1);
    let length_squared = dx * dx + dy * dy;
    let (px, py) = (x as f32 - SLASH_START.0, y as f32 - SLASH_START.1);
    let along = (px * dx + py * dy) / length_squared;
    let distance = (dx * py - dy * px).abs() / length_squared.sqrt();
    if (0.0..=1.0).contains(&along) && distance <= SLASH_HALF_WIDTH {
        Some(GlyphPart::Slash)
    } else if (-0.05..=1.05).contains(&along) && distance <= SLASH_CLEARANCE {
        None
    } else {
        glyph_part(x, y)
    }
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
    fn muted_glyph_draws_a_slash_across_the_microphone() {
        // A point on the slash, where the live glyph is empty.
        assert_eq!(glyph_part(26, 6), None);
        assert_eq!(muted_glyph_part(26, 6), Some(GlyphPart::Slash));
    }

    #[test]
    fn muted_glyph_clears_a_gap_beside_the_slash() {
        // Capsule pixel just off the slash line: drawn when live, cleared when muted.
        assert_eq!(glyph_part(16, 13), Some(GlyphPart::Capsule));
        assert_eq!(muted_glyph_part(16, 13), None);
    }

    #[test]
    fn muted_glyph_keeps_the_rest_of_the_microphone() {
        assert_eq!(muted_glyph_part(12, 8), Some(GlyphPart::Capsule));
        assert_eq!(muted_glyph_part(12, 26), Some(GlyphPart::Stand));
    }

    #[test]
    fn rounded_corners_and_margins_stay_transparent() {
        assert_eq!(glyph_part(0, 0), None);
        assert_eq!(glyph_part(11, 3), None);
        assert_eq!(glyph_part(GLYPH_SIZE - 1, GLYPH_SIZE - 1), None);
    }
}
