//! Pure logic of the virtual list: height table, converting scroll position to render range.

use std::collections::HashMap;
use std::ops::Range;

/// How close to the list top / bottom triggers loading more.
const REACH_THRESHOLD: f64 = 400.0;
/// Number of items to render when there is no real viewport height.
const INITIAL_ITEMS: usize = 30;
/// Minimum samples needed before using the measured average as the estimated height.
const MIN_SAMPLES: usize = 8;

/// Fenwick tree: prefix sums and offset lookup are both O(log n); per-frame cost is negligible for
/// a few thousand items.
#[derive(Debug, Default)]
struct HeightTree {
    tree: Vec<f64>,
    heights: Vec<f64>,
}

impl HeightTree {
    fn build(heights: Vec<f64>) -> Self {
        let mut tree = vec![0.0; heights.len() + 1];
        for (i, h) in heights.iter().enumerate() {
            let pos = i + 1;
            tree[pos] += h;
            let parent = pos + pos.isolate_lowest_one();
            if parent < tree.len() {
                let value = tree[pos];
                tree[parent] += value;
            }
        }
        Self { tree, heights }
    }

    fn set(&mut self, index: usize, height: f64) {
        let delta = height - self.heights[index];
        self.heights[index] = height;
        let mut pos = index + 1;
        while pos < self.tree.len() {
            self.tree[pos] += delta;
            pos += pos.isolate_lowest_one();
        }
    }

    /// Sum of the heights of the first `count` items.
    fn prefix(&self, count: usize) -> f64 {
        let mut sum = 0.0;
        let mut pos = count;
        while pos > 0 {
            sum += self.tree[pos];
            pos &= pos - 1;
        }
        sum
    }

    fn total(&self) -> f64 {
        self.prefix(self.heights.len())
    }

    /// The item containing offset `y`.
    fn index_at(&self, y: f64) -> usize {
        let len = self.heights.len();
        if len == 0 || y <= 0.0 {
            return 0;
        }
        let mut pos = 0;
        let mut remaining = y;
        let mut step = len.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= len && self.tree[next] <= remaining {
                pos = next;
                remaining -= self.tree[next];
            }
            step >>= 1;
        }
        pos.min(len - 1)
    }
}

/// Scroll state reported by JS.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScrollReport {
    pub top: f64,
    pub view: f64,
    pub at_bottom: bool,
    /// The item at the top of the viewport, and the distance from that item's top.
    pub anchor: Option<(u64, f64)>,
}

/// Events for being near either end of the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reached {
    pub top: bool,
    pub bottom: bool,
}

#[derive(Debug)]
pub struct ListState {
    keys: Vec<u64>,
    index: HashMap<u64, usize>,
    measured: HashMap<u64, f64>,
    tree: HeightTree,
    estimate: f64,
    scroll: ScrollReport,
    range: Range<usize>,
    overscan: f64,
    stick: bool,
    top_armed: bool,
    bottom_armed: bool,
}

impl ListState {
    pub fn new(estimate: f64, overscan: f64, stick: bool, start_at_bottom: bool) -> Self {
        Self {
            keys: Vec::new(),
            index: HashMap::new(),
            measured: HashMap::new(),
            tree: HeightTree::default(),
            estimate,
            scroll: ScrollReport {
                at_bottom: start_at_bottom,
                ..Default::default()
            },
            range: 0..0,
            overscan,
            stick,
            top_armed: true,
            bottom_armed: true,
        }
    }

    pub fn keys(&self) -> &[u64] {
        &self.keys
    }

    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Height of the placeholders above and below the render range.
    pub fn spacers(&self) -> (f64, f64) {
        let top = self.tree.prefix(self.range.start);
        (top, self.tree.total() - self.tree.prefix(self.range.end))
    }

    pub fn stick(&self) -> bool {
        self.stick
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll.at_bottom
    }

    /// The item at the bottom of the viewport.
    pub fn last_visible(&self) -> Option<u64> {
        let y = self.scroll.top + self.scroll.view - 1.0;
        self.keys.get(self.tree.index_at(y)).copied()
    }

    /// Replace the item list. If the anchor item is still there, convert the scroll position to its
    /// new position (keeps position when loading history upward).
    pub fn set_keys(&mut self, keys: &[u64]) {
        let first_changed = self.keys.first() != keys.first();
        let last_changed = self.keys.last() != keys.last();
        self.keys = keys.to_vec();
        self.index = keys.iter().enumerate().map(|(i, k)| (*k, i)).collect();
        self.measured.retain(|k, _| self.index.contains_key(k));
        if self.measured.len() >= MIN_SAMPLES {
            self.estimate = self.measured.values().sum::<f64>() / self.measured.len() as f64;
        }
        let heights = keys
            .iter()
            .map(|k| self.measured.get(k).copied().unwrap_or(self.estimate))
            .collect();
        self.tree = HeightTree::build(heights);
        self.top_armed |= first_changed;
        self.bottom_armed |= last_changed;

        if self.scroll.at_bottom && (self.stick || self.range.end >= self.keys.len()) {
            self.scroll.top = (self.tree.total() - self.scroll.view).max(0.0);
        } else if let Some((key, off)) = self.scroll.anchor
            && let Some(&i) = self.index.get(&key)
        {
            self.scroll.top = self.tree.prefix(i) + off;
        }
        self.scroll.top = self.scroll.top.clamp(0.0, self.tree.total());
        self.range = self.compute_range();
    }

    /// Record measured heights; returns true if the render range changed as a result.
    pub fn measure(&mut self, sizes: &[(u64, f64)]) -> bool {
        for &(key, height) in sizes {
            if let Some(&i) = self.index.get(&key) {
                self.measured.insert(key, height);
                self.tree.set(i, height);
            }
        }
        self.refresh_range()
    }

    /// Record scroll state; returns true if the render range changed as a result.
    pub fn on_scroll(&mut self, report: ScrollReport) -> bool {
        self.scroll = report;
        self.refresh_range()
    }

    /// Fires once per end when near it, until the list changes at that end or the user leaves.
    pub fn take_reached(&mut self) -> Reached {
        let total = self.tree.total();
        let top = self.scroll.top;
        let bottom_gap = total - top - self.scroll.view;
        let mut reached = Reached::default();
        // Do not fire while the viewport is still in the placeholder area (items not rendered):
        // appending content then cannot preserve the position
        if self.keys.is_empty() || self.scroll.view <= 0.0 || self.scroll.anchor.is_none() {
            return reached;
        }
        if top < REACH_THRESHOLD {
            reached.top = std::mem::take(&mut self.top_armed);
        } else if top > REACH_THRESHOLD * 2.0 {
            self.top_armed = true;
        }
        if bottom_gap < REACH_THRESHOLD {
            reached.bottom = std::mem::take(&mut self.bottom_armed);
        } else if bottom_gap > REACH_THRESHOLD * 2.0 {
            self.bottom_armed = true;
        }
        reached
    }

    /// Jump to an item: the render range moves near it immediately. Returns false if the item does
    /// not exist.
    pub fn jump_to(&mut self, key: u64) -> bool {
        let Some(&i) = self.index.get(&key) else {
            return false;
        };
        self.scroll.top = self.tree.prefix(i);
        self.scroll.anchor = Some((key, 0.0));
        self.scroll.at_bottom = false;
        self.range = self.compute_range();
        true
    }

    pub fn jump_to_bottom(&mut self) {
        self.scroll.top = (self.tree.total() - self.scroll.view).max(0.0);
        self.scroll.anchor = None;
        self.scroll.at_bottom = true;
        self.range = self.compute_range();
    }

    fn refresh_range(&mut self) -> bool {
        let len = self.keys.len();
        let view = self.scroll.view;
        let top = self.scroll.top;
        let margin = self.overscan / 2.0;
        // Leave it alone while the current range still covers "viewport + half the pre-render
        // area", to avoid re-rendering every frame
        let need_start = self.tree.index_at(top - margin);
        let need_end = (self.tree.index_at(top + view + margin) + 1).min(len);
        let tail_needed = self.scroll.at_bottom && self.range.end < len;
        if self.range.start <= need_start
            && self.range.end >= need_end
            && !tail_needed
            && !self.range.is_empty()
        {
            return false;
        }
        let next = self.compute_range();
        let changed = next != self.range;
        self.range = next;
        changed
    }

    fn compute_range(&self) -> Range<usize> {
        let len = self.keys.len();
        if len == 0 {
            return 0..0;
        }
        if self.scroll.view <= 0.0 {
            return if self.scroll.at_bottom {
                len.saturating_sub(INITIAL_ITEMS)..len
            } else {
                let start = self
                    .tree
                    .index_at(self.scroll.top)
                    .saturating_sub(INITIAL_ITEMS / 2);
                start..(start + INITIAL_ITEMS).min(len)
            };
        }
        let start = self.tree.index_at(self.scroll.top - self.overscan);
        let mut end = (self
            .tree
            .index_at(self.scroll.top + self.scroll.view + self.overscan)
            + 1)
        .min(len);
        if self.scroll.at_bottom {
            end = len;
        }
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(range: Range<u64>) -> Vec<u64> {
        range.collect()
    }

    #[test]
    fn tree_prefix_and_lookup() {
        let mut t = HeightTree::build(vec![10.0, 20.0, 30.0, 40.0, 50.0]);
        assert_eq!(t.prefix(0), 0.0);
        assert_eq!(t.prefix(3), 60.0);
        assert_eq!(t.total(), 150.0);
        assert_eq!(t.index_at(-5.0), 0);
        assert_eq!(t.index_at(9.0), 0);
        assert_eq!(t.index_at(10.0), 1);
        assert_eq!(t.index_at(59.0), 2);
        assert_eq!(t.index_at(1000.0), 4);
        t.set(1, 120.0);
        assert_eq!(t.prefix(2), 130.0);
        assert_eq!(t.index_at(129.0), 1);
        assert_eq!(t.index_at(130.0), 2);
    }

    #[test]
    fn empty_list_has_empty_range() {
        let mut s = ListState::new(50.0, 100.0, false, false);
        s.set_keys(&[]);
        assert_eq!(s.range(), 0..0);
        assert_eq!(s.spacers(), (0.0, 0.0));
    }

    #[test]
    fn initial_range_without_viewport() {
        let mut s = ListState::new(50.0, 100.0, false, false);
        s.set_keys(&keys(0..1000));
        assert_eq!(s.range(), 0..INITIAL_ITEMS);
        let mut s = ListState::new(50.0, 100.0, true, true);
        s.set_keys(&keys(0..1000));
        assert_eq!(s.range(), 970..1000);
    }

    #[test]
    fn range_follows_scroll_with_hysteresis() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..1000));
        assert!(s.on_scroll(ScrollReport {
            top: 5000.0,
            view: 500.0,
            ..Default::default()
        }));
        assert_eq!(s.range(), 96..115);
        // Scrolling within half the pre-render area does not change the range
        assert!(!s.on_scroll(ScrollReport {
            top: 5050.0,
            view: 500.0,
            ..Default::default()
        }));
        assert!(s.on_scroll(ScrollReport {
            top: 5200.0,
            view: 500.0,
            ..Default::default()
        }));
        let (top, bottom) = s.spacers();
        assert_eq!(top, s.range().start as f64 * 50.0);
        assert_eq!(bottom, (1000 - s.range().end) as f64 * 50.0);
    }

    #[test]
    fn prepend_keeps_anchor_position() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(100..200));
        s.on_scroll(ScrollReport {
            top: 500.0,
            view: 400.0,
            anchor: Some((110, 0.0)),
            ..Default::default()
        });
        s.set_keys(&keys(0..200));
        // 100 items added in front, 50 each
        let range = s.range();
        assert!(range.contains(&110));
        assert!(range.contains(&118));
        assert_eq!(s.spacers().0, range.start as f64 * 50.0);
    }

    #[test]
    fn append_while_at_bottom_includes_tail() {
        let mut s = ListState::new(50.0, 200.0, true, true);
        s.set_keys(&keys(0..100));
        s.on_scroll(ScrollReport {
            top: 4600.0,
            view: 400.0,
            at_bottom: true,
            anchor: None,
        });
        s.set_keys(&keys(0..105));
        assert_eq!(s.range().end, 105);
    }

    #[test]
    fn append_while_scrolled_up_stays() {
        let mut s = ListState::new(50.0, 200.0, true, false);
        s.set_keys(&keys(0..100));
        s.on_scroll(ScrollReport {
            top: 1000.0,
            view: 400.0,
            at_bottom: false,
            anchor: Some((20, 0.0)),
        });
        let before = s.range();
        s.set_keys(&keys(0..105));
        assert_eq!(s.range(), before);
    }

    #[test]
    fn measure_updates_spacers_and_keeps_entries_across_key_changes() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..100));
        s.on_scroll(ScrollReport {
            top: 2000.0,
            view: 400.0,
            ..Default::default()
        });
        let start = s.range().start;
        s.measure(&[(0, 150.0)]);
        assert_eq!(s.spacers().0, start as f64 * 50.0 + 100.0);
        s.set_keys(&keys(0..101));
        assert_eq!(s.tree.heights[0], 150.0);
    }

    #[test]
    fn measure_ignores_unknown_keys() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..10));
        assert!(!s.measure(&[(999, 10.0)]));
    }

    #[test]
    fn estimate_adapts_to_measured_average() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..100));
        let sizes: Vec<(u64, f64)> = (0..10).map(|k| (k, 100.0)).collect();
        s.measure(&sizes);
        s.set_keys(&keys(0..101));
        assert_eq!(s.tree.total(), 10100.0);
    }

    #[test]
    fn reached_fires_once_until_list_changes() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(100..200));
        s.on_scroll(ScrollReport {
            top: 100.0,
            view: 400.0,
            anchor: Some((1, 0.0)),
            ..Default::default()
        });
        assert!(s.take_reached().top);
        s.on_scroll(ScrollReport {
            top: 50.0,
            view: 400.0,
            anchor: Some((101, 0.0)),
            ..Default::default()
        });
        assert!(!s.take_reached().top);
        s.set_keys(&keys(50..200));
        s.on_scroll(ScrollReport {
            top: 50.0,
            view: 400.0,
            anchor: Some((101, 0.0)),
            ..Default::default()
        });
        assert!(s.take_reached().top);
    }

    #[test]
    fn reached_rearms_after_leaving() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..1000));
        s.on_scroll(ScrollReport {
            top: 100.0,
            view: 400.0,
            anchor: Some((1, 0.0)),
            ..Default::default()
        });
        assert!(s.take_reached().top);
        s.on_scroll(ScrollReport {
            top: 2000.0,
            view: 400.0,
            anchor: Some((40, 0.0)),
            ..Default::default()
        });
        assert!(!s.take_reached().top);
        s.on_scroll(ScrollReport {
            top: 100.0,
            view: 400.0,
            anchor: Some((1, 0.0)),
            ..Default::default()
        });
        assert!(s.take_reached().top);
    }

    #[test]
    fn reached_waits_until_items_are_rendered() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(100..200));
        s.on_scroll(ScrollReport {
            top: 0.0,
            view: 400.0,
            anchor: None,
            ..Default::default()
        });
        assert!(!s.take_reached().top);
        s.on_scroll(ScrollReport {
            top: 0.0,
            view: 400.0,
            anchor: Some((100, 0.0)),
            ..Default::default()
        });
        assert!(s.take_reached().top);
    }

    #[test]
    fn reached_bottom() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..100));
        s.on_scroll(ScrollReport {
            top: 4500.0,
            view: 400.0,
            anchor: Some((90, 0.0)),
            ..Default::default()
        });
        assert!(s.take_reached().bottom);
        assert!(!s.take_reached().bottom);
    }

    #[test]
    fn jump_moves_range_to_target() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..1000));
        s.on_scroll(ScrollReport {
            top: 0.0,
            view: 400.0,
            ..Default::default()
        });
        assert!(s.jump_to(500));
        assert!(s.range().contains(&500));
        assert!(!s.jump_to(5000));
        s.jump_to_bottom();
        assert_eq!(s.range().end, 1000);
        assert!(s.is_at_bottom());
    }

    #[test]
    fn last_visible_follows_viewport() {
        let mut s = ListState::new(50.0, 200.0, false, false);
        s.set_keys(&keys(0..100));
        s.on_scroll(ScrollReport {
            top: 500.0,
            view: 200.0,
            anchor: Some((10, 0.0)),
            ..Default::default()
        });
        assert_eq!(s.last_visible(), Some(13));
    }
}
