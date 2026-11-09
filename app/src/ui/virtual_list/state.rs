//! 虚拟列表的纯逻辑：高度表、滚动位置到渲染范围的换算。

use std::collections::HashMap;
use std::ops::Range;

/// 距列表顶部 / 底部多近时触发加载更多。
const REACH_THRESHOLD: f64 = 400.0;
/// 没有实际视口高度时渲染的条数。
const INITIAL_ITEMS: usize = 30;
/// 用实测平均值作为估计高度所需的最少样本。
const MIN_SAMPLES: usize = 8;

/// 树状数组：前缀和、按偏移查找都是 O(log n)，几千条下每帧计算没有压力。
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

    /// 前 `count` 项的高度和。
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

    /// 包含偏移 `y` 的条目。
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

/// JS 报告的滚动状态。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScrollReport {
    pub top: f64,
    pub view: f64,
    pub at_bottom: bool,
    /// 视口顶端所在的条目，以及距条目顶端的距离。
    pub anchor: Option<(u64, f64)>,
}

/// 接近列表两端的事件。
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

    /// 渲染范围上下占位的高度。
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

    /// 视口底端所在的条目。
    pub fn last_visible(&self) -> Option<u64> {
        let y = self.scroll.top + self.scroll.view - 1.0;
        self.keys.get(self.tree.index_at(y)).copied()
    }

    /// 更换条目列表。锚点条目还在时，滚动位置换算到它的新位置（向上加载历史时保持位置）。
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

    /// 记录实测高度；渲染范围因此改变时返回 true。
    pub fn measure(&mut self, sizes: &[(u64, f64)]) -> bool {
        for &(key, height) in sizes {
            if let Some(&i) = self.index.get(&key) {
                self.measured.insert(key, height);
                self.tree.set(i, height);
            }
        }
        self.refresh_range()
    }

    /// 记录滚动状态；渲染范围因此改变时返回 true。
    pub fn on_scroll(&mut self, report: ScrollReport) -> bool {
        self.scroll = report;
        self.refresh_range()
    }

    /// 接近两端时各触发一次，直到列表在该端有变化或用户离开。
    pub fn take_reached(&mut self) -> Reached {
        let total = self.tree.total();
        let top = self.scroll.top;
        let bottom_gap = total - top - self.scroll.view;
        let mut reached = Reached::default();
        // 视口还落在占位区（条目没渲染出来）时不触发：此时追加内容无法保持位置
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

    /// 跳到某条目：渲染范围立刻移到它附近。条目不存在时返回 false。
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
        // 当前范围还覆盖“视口 + 半个预渲染区”时不动，避免每帧重新渲染
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
        // 在半个预渲染区内滚动不改变范围
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
        // 前面多了 100 条，每条 50
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
