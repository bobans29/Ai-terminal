//! Shared tab bar layout and hit testing.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Select(usize),
    Close(usize),
    New,
}

const MAX_TAB_CELLS: f32 = 24.0;
const NEW_BUTTON_CELLS: f32 = 3.0;
const CLOSE_CELLS: f32 = 2.0;

fn dimensions(width: f32, cell_w: f32, count: usize) -> (f32, f32) {
    if !width.is_finite() || !cell_w.is_finite() || width <= 0.0 || cell_w <= 0.0 {
        return (0.0, 0.0);
    }
    let new_w = width.min(NEW_BUTTON_CELLS * cell_w);
    let tabs_w = (width - new_w).max(0.0);
    let tab_w = if count == 0 {
        0.0
    } else {
        (tabs_w / count as f32).min(MAX_TAB_CELLS * cell_w)
    };
    (tab_w, new_w)
}

pub fn tab_width(width: f32, cell_w: f32, count: usize) -> f32 {
    dimensions(width, cell_w, count).0
}

pub fn new_button_x(width: f32, cell_w: f32, count: usize) -> f32 {
    let (tab_w, new_w) = dimensions(width, cell_w, count);
    // Keep the button after the tabs when capped widths leave unused space.
    (tab_w * count as f32).min(width - new_w).max(0.0)
}

pub fn hit(x: f32, width: f32, cell_w: f32, count: usize) -> Option<Hit> {
    if !x.is_finite() || x < 0.0 || x >= width || width <= 0.0 || cell_w <= 0.0 {
        return None;
    }
    let (tab_w, new_w) = dimensions(width, cell_w, count);
    let tabs_right = tab_w * count as f32;
    if new_w > 0.0 && x >= new_button_x(width, cell_w, count) && x < tabs_right + new_w {
        return Some(Hit::New);
    }
    if tab_w <= 0.0 || x >= tabs_right {
        return None;
    }
    let index = (x / tab_w).floor() as usize;
    if index >= count {
        return None;
    }
    let within = x - index as f32 * tab_w;
    if tab_w >= CLOSE_CELLS * cell_w && within >= tab_w - CLOSE_CELLS * cell_w {
        Some(Hit::Close(index))
    } else {
        Some(Hit::Select(index))
    }
}

/// Reserve a small profile selector at the right of the tab bar.
pub fn profile_width(width: f32, cell_w: f32) -> f32 {
    if !width.is_finite() || !cell_w.is_finite() || width <= 0.0 || cell_w <= 0.0 {
        return 0.0;
    }
    (24.0 * cell_w).min(width / 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_reserves_new_button_caps_tabs_and_hits_close_regions() {
        let (width, cell) = (1000.0, 10.0);
        assert_eq!(tab_width(width, cell, 2), 240.0);
        assert_eq!(new_button_x(width, cell, 2), 480.0);
        assert_eq!(hit(100.0, width, cell, 2), Some(Hit::Select(0)));
        assert_eq!(hit(230.0, width, cell, 2), Some(Hit::Close(0)));
        assert_eq!(hit(490.0, width, cell, 2), Some(Hit::New));
        assert_eq!(hit(999.0, width, cell, 2), None);
    }

    #[test]
    fn narrow_geometry_is_safe_and_does_not_expose_close_hits() {
        assert_eq!(tab_width(20.0, 10.0, 4), 0.0);
        assert_eq!(hit(3.0, 20.0, 10.0, 4), Some(Hit::New));
        assert_eq!(hit(19.0, 20.0, 10.0, 4), Some(Hit::New));
        assert_eq!(hit(-1.0, 20.0, 10.0, 4), None);
        assert_eq!(hit(3.0, 100.0, 0.0, 4), None);
        assert_eq!(tab_width(100.0, 10.0, 0), 0.0);
    }
}
