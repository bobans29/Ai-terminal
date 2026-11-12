//! Binary pane layout. Session IDs remain stable when a sibling is removed.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn contains(self, x: f64, y: f64) -> bool {
        x.is_finite()
            && y.is_finite()
            && x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x < f64::from(self.x) + f64::from(self.width)
            && y < f64::from(self.y) + f64::from(self.height)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Axis {
    /// A vertical divider, with panes beside one another.
    Vertical,
    /// A horizontal divider, with panes above and below one another.
    Horizontal,
}

#[derive(Debug)]
pub enum Layout {
    Leaf(u64),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn rects(&self, bounds: Rect, gap: u32) -> Vec<(u64, Rect)> {
        let mut rects = Vec::new();
        self.collect_rects(bounds, gap, &mut rects);
        rects
    }

    fn collect_rects(&self, bounds: Rect, gap: u32, rects: &mut Vec<(u64, Rect)>) {
        match self {
            Self::Leaf(id) => rects.push((*id, bounds)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, _, b) = partition(bounds, *axis, *ratio, gap);
                first.collect_rects(a, gap, rects);
                second.collect_rects(b, gap, rects);
            }
        }
    }

    pub fn split(&mut self, target: u64, new: u64, axis: Axis) -> bool {
        match self {
            Self::Leaf(id) if *id == target => {
                *self = Self::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Self::Leaf(target)),
                    second: Box::new(Self::Leaf(new)),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                first.split(target, new, axis) || second.split(target, new, axis)
            }
        }
    }

    /// Remove a leaf and collapse its parent. The final leaf belongs to its
    /// tab; returning false for that leaf lets the caller close the tab.
    pub fn remove(&mut self, id: u64) -> bool {
        let Self::Split { first, second, .. } = self else {
            return false;
        };
        let remove_first = matches!(first.as_ref(), Self::Leaf(leaf) if *leaf == id);
        let remove_second = matches!(second.as_ref(), Self::Leaf(leaf) if *leaf == id);
        if remove_first || remove_second {
            let old = std::mem::replace(self, Self::Leaf(id));
            if let Self::Split { first, second, .. } = old {
                *self = if remove_first { *second } else { *first };
            }
            true
        } else {
            first.remove(id) || second.remove(id)
        }
    }

    pub fn min_size(&self, leaf_w: u32, leaf_h: u32, gap: u32) -> (u32, u32) {
        match self {
            Self::Leaf(_) => (leaf_w, leaf_h),
            Self::Split {
                axis,
                first,
                second,
                ..
            } => {
                let (aw, ah) = first.min_size(leaf_w, leaf_h, gap);
                let (bw, bh) = second.min_size(leaf_w, leaf_h, gap);
                match axis {
                    Axis::Vertical => (aw.saturating_add(bw).saturating_add(gap), ah.max(bh)),
                    Axis::Horizontal => (aw.max(bw), ah.saturating_add(bh).saturating_add(gap)),
                }
            }
        }
    }

    /// A path uses false for the first child and true for the second.
    pub fn divider_at(&self, bounds: Rect, gap: u32, x: f64, y: f64) -> Option<Vec<bool>> {
        let Self::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return None;
        };
        let (a, divider, b) = partition(bounds, *axis, *ratio, gap);
        if divider.contains(x, y) {
            return Some(Vec::new());
        }
        if a.contains(x, y) {
            let mut path = first.divider_at(a, gap, x, y)?;
            path.insert(0, false);
            Some(path)
        } else if b.contains(x, y) {
            let mut path = second.divider_at(b, gap, x, y)?;
            path.insert(0, true);
            Some(path)
        } else {
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resize_divider(
        &mut self,
        path: &[bool],
        bounds: Rect,
        gap: u32,
        x: f64,
        y: f64,
        leaf_w: u32,
        leaf_h: u32,
    ) -> bool {
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        let Self::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return false;
        };
        if let Some((&next, rest)) = path.split_first() {
            let (a, _, b) = partition(bounds, *axis, *ratio, gap);
            return if next {
                second.resize_divider(rest, b, gap, x, y, leaf_w, leaf_h)
            } else {
                first.resize_divider(rest, a, gap, x, y, leaf_w, leaf_h)
            };
        }
        let (aw, ah) = first.min_size(leaf_w, leaf_h, gap);
        let (bw, bh) = second.min_size(leaf_w, leaf_h, gap);
        let (length, start, pointer, min_a, min_b) = match axis {
            Axis::Vertical => (bounds.width, bounds.x, x, aw, bw),
            Axis::Horizontal => (bounds.height, bounds.y, y, ah, bh),
        };
        let actual_gap = gap.min(length);
        let available = length - actual_gap;
        if available == 0 || u64::from(min_a) + u64::from(min_b) > u64::from(available) {
            // Shrinking a window must not destroy the stored ratio. Dragging
            // resumes once both subtrees have room for their minimum sizes.
            return false;
        }
        let desired = pointer - f64::from(start) - f64::from(actual_gap) / 2.0;
        let pixels = desired.clamp(f64::from(min_a), f64::from(available - min_b));
        let new_ratio = (pixels / f64::from(available)) as f32;
        if (*ratio - new_ratio).abs() <= f32::EPSILON {
            return false;
        }
        *ratio = new_ratio;
        true
    }
}

fn partition(bounds: Rect, axis: Axis, ratio: f32, gap: u32) -> (Rect, Rect, Rect) {
    let length = match axis {
        Axis::Vertical => bounds.width,
        Axis::Horizontal => bounds.height,
    };
    let gap = gap.min(length);
    let available = length - gap;
    let ratio = if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let first_len = (f64::from(available) * f64::from(ratio)).round() as u32;
    let second_len = available - first_len;
    let mut first = bounds;
    let mut divider = bounds;
    let mut second = bounds;
    match axis {
        Axis::Vertical => {
            first.width = first_len;
            divider.x = bounds.x.saturating_add(first_len);
            divider.width = gap;
            second.x = divider.x.saturating_add(gap);
            second.width = second_len;
        }
        Axis::Horizontal => {
            first.height = first_len;
            divider.y = bounds.y.saturating_add(first_len);
            divider.height = gap;
            second.y = divider.y.saturating_add(gap);
            second.height = second_len;
        }
    }
    (first, divider, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(width: u32, height: u32) -> Rect {
        Rect {
            x: 10,
            y: 20,
            width,
            height,
        }
    }

    #[test]
    fn nested_splits_cover_available_space_and_collapse_to_surviving_ids() {
        let mut layout = Layout::Leaf(1);
        assert!(layout.split(1, 2, Axis::Vertical));
        assert!(layout.split(2, 3, Axis::Horizontal));
        assert!(!layout.split(99, 4, Axis::Vertical));
        assert_eq!(
            layout.rects(bounds(104, 84), 4),
            vec![
                (
                    1,
                    Rect {
                        x: 10,
                        y: 20,
                        width: 50,
                        height: 84
                    }
                ),
                (
                    2,
                    Rect {
                        x: 64,
                        y: 20,
                        width: 50,
                        height: 40
                    }
                ),
                (
                    3,
                    Rect {
                        x: 64,
                        y: 64,
                        width: 50,
                        height: 40
                    }
                ),
            ]
        );
        assert!(layout.remove(2));
        assert_eq!(
            layout.rects(bounds(104, 84), 4)[1],
            (
                3,
                Rect {
                    x: 64,
                    y: 20,
                    width: 50,
                    height: 84
                }
            )
        );
        assert!(layout.remove(1));
        assert_eq!(layout.rects(bounds(104, 84), 4), vec![(3, bounds(104, 84))]);
        assert!(!layout.remove(3));
        assert!(!layout.remove(99));
    }

    #[test]
    fn divider_hit_paths_and_resize_respect_nested_minimum_sizes() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Vertical);
        layout.split(2, 3, Axis::Horizontal);
        let area = bounds(204, 104);
        assert_eq!(layout.min_size(30, 20, 4), (64, 44));
        assert_eq!(layout.divider_at(area, 4, 111.0, 30.0), Some(vec![]));
        assert_eq!(layout.divider_at(area, 4, 150.0, 71.0), Some(vec![true]));
        assert_eq!(layout.divider_at(area, 4, 15.0, 30.0), None);
        assert!(layout.resize_divider(&[], area, 4, -100.0, 0.0, 30, 20));
        assert_eq!(layout.rects(area, 4)[0].1.width, 30);
        assert!(layout.resize_divider(&[true], area, 4, 0.0, 1000.0, 30, 20));
        let rects = layout.rects(area, 4);
        assert_eq!(rects[1].1.height, 80);
        assert_eq!(rects[2].1.height, 20);
        assert!(!layout.resize_divider(&[false], area, 4, 0.0, 0.0, 30, 20));
    }

    #[test]
    fn tiny_windows_do_not_overlap_or_destroy_ratios() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Vertical);
        assert!(layout.resize_divider(&[], bounds(104, 80), 4, 87.0, 30.0, 10, 10));
        let before = layout.rects(bounds(104, 80), 4);
        for width in [0, 1, 3, 4, 5] {
            let rects = layout.rects(bounds(width, 0), 4);
            assert!(rects.iter().all(|(_, r)| r.width <= width && r.height == 0));
            assert!(rects[0].1.x + rects[0].1.width <= rects[1].1.x);
            assert!(rects[1].1.x + rects[1].1.width <= 10 + width);
            assert!(!layout.resize_divider(&[], bounds(width, 0), 4, 0.0, 0.0, 10, 10));
        }
        assert_eq!(layout.rects(bounds(104, 80), 4), before);
        assert!(!bounds(0, 0).contains(10.0, 20.0));
        assert!(!bounds(10, 10).contains(f64::NAN, 20.0));
    }
}
