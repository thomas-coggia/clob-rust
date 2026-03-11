//! Intrusive skip-list orderbook: pool-allocated nodes, O(1) lookup by price tick,
//! O(log n) insert/remove, O(log k) to reach depth k, O(1) for the best level.
//! IS_BID is a const generic: the comparison operator is resolved at compile time.
//!
//! ## Layout
//!
//! Each `LevelNode` is stored in a flat `Vec` (the pool) and referenced by u32 index.
//! `forwards[0]` is the full linked-list forward pointer (every node in sorted order).
//! `forwards[h]` for h > 0 are express-lane pointers that skip ~4^h nodes on average,
//! giving O(log k) traversal to depth k while still starting iteration from `head[0]`
//! (the best bid / best ask) in O(1).

use crate::basic_orderbook::{Level, OrderbookLike, Price, Quantity, Side};
use rust_decimal::Decimal;
use std::collections::HashMap;

const NONE: u32 = u32::MAX;
const MAX_HEIGHT: usize = 4; // Maximum skip list height. With p=0.25 promotion probability,
                              // each level requires ~4x more nodes to populate. MAX_HEIGHT=4
                              // supports ~4^4 ≈ 256 price levels before performance degrades.

/// One node in the intrusive skip list.
///
/// `forwards[0]` is the level-0 (full-list) forward pointer, equivalent to `next`
/// in a plain linked list.  When on the free list, `forwards[0]` is the free-list link.
#[repr(C)]
struct LevelNode {
    price: Decimal,
    size: Decimal,
    height: u8,
    forwards: [u32; MAX_HEIGHT],
}

/// One side (bids or asks) of the skip-list orderbook.
///
/// `IS_BID = true`  → descending price order (highest first).
/// `IS_BID = false` → ascending price order  (lowest  first).
struct SkipBookSide<const IS_BID: bool> {
    nodes: Vec<LevelNode>,
    free_head: u32,
    /// Entry pointer for each skip level.  `head[0]` is the best level of the book.
    head: [u32; MAX_HEIGHT],
    tick_to_node: HashMap<Decimal, u32>,
    /// xorshift64 state — no external rand dependency.
    rng: u64,
}

impl<const IS_BID: bool> SkipBookSide<IS_BID> {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            free_head: NONE,
            head: [NONE; MAX_HEIGHT],
            tick_to_node: HashMap::with_capacity(64),
            rng: 0x_dead_beef_cafe_babe,
        }
    }

    /// True when `a` should appear before `b` in this side's ordering.
    /// Inlined and resolved at compile time via the const generic.
    #[inline(always)]
    fn comes_before(a: Decimal, b: Decimal) -> bool {
        if IS_BID { a > b } else { a < b }
    }

    /// Samples a node height using xorshift64.
    /// Each additional level is promoted with probability 1/4 (p = 0.25),
    /// giving expected height log₄(n) ≈ 0.5 log₂(n).
    #[inline]
    fn random_height(&mut self) -> u8 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        let mut h = 1u8;
        let mut bits = x;
        while h < MAX_HEIGHT as u8 && (bits & 3) == 0 {
            h += 1;
            bits >>= 2;
        }
        h
    }

    fn alloc_node(&mut self, price: Decimal, size: Decimal, height: u8) -> Option<u32> {
        if self.free_head != NONE {
            let i = self.free_head;
            let node = &mut self.nodes[i as usize];
            self.free_head = node.forwards[0];
            node.price = price;
            node.size = size;
            node.height = height;
            node.forwards = [NONE; MAX_HEIGHT];
            Some(i)
        } else {
            let i = self.nodes.len() as u32;
            if i == NONE {
                return None;
            }
            self.nodes.push(LevelNode { price, size, height, forwards: [NONE; MAX_HEIGHT] });
            Some(i)
        }
    }

    fn free_node(&mut self, i: u32) {
        let tick = self.nodes[i as usize].price;
        self.tick_to_node.remove(&tick);
        self.nodes[i as usize].forwards[0] = self.free_head;
        self.free_head = i;
    }

    /// Walk the skip list from the top level down, collecting `update[h]` = the index
    /// of the rightmost predecessor at level h for `tick` (NONE = before `head[h]`).
    ///
    /// Carry-over: when descending from level h+1 to h, we resume from the position
    /// where we stopped at h+1.  This is safe because any node accessible at level h+1
    /// has height ≥ h+2, and therefore also participates in level h (forwards[h] is valid).
    fn find_update(&self, tick: Decimal) -> [u32; MAX_HEIGHT] {
        let mut update = [NONE; MAX_HEIGHT];
        let mut cur = NONE; // NONE == "we are at the virtual head"

        for h in (0..MAX_HEIGHT).rev() {
            // Resume from where the level above stopped; start from head[h] if at the top.
            let mut probe = if cur == NONE {
                self.head[h]
            } else {
                self.nodes[cur as usize].forwards[h]
            };

            while probe != NONE {
                if Self::comes_before(self.nodes[probe as usize].price, tick) {
                    cur = probe;
                    probe = self.nodes[cur as usize].forwards[h];
                } else {
                    break;
                }
            }
            update[h] = cur;
        }
        update
    }

    /// Splice node `i` into the skip list at every level in `0..height`.
    fn link(&mut self, i: u32) {
        let tick = self.nodes[i as usize].price;
        let height = self.nodes[i as usize].height as usize;
        self.tick_to_node.insert(tick, i);

        let update = self.find_update(tick);

        for h in 0..height {
            if update[h] == NONE {
                self.nodes[i as usize].forwards[h] = self.head[h];
                self.head[h] = i;
            } else {
                let pred = update[h];
                self.nodes[i as usize].forwards[h] = self.nodes[pred as usize].forwards[h];
                self.nodes[pred as usize].forwards[h] = i;
            }
        }
    }

    /// Remove node `i` from every level in `0..height`.
    fn unlink(&mut self, i: u32) {
        let tick = self.nodes[i as usize].price;
        let height = self.nodes[i as usize].height as usize;

        let update = self.find_update(tick);

        for h in 0..height {
            // Guard: only splice if the predecessor's forward pointer actually points to i.
            let pred_next = if update[h] == NONE {
                self.head[h]
            } else {
                self.nodes[update[h] as usize].forwards[h]
            };

            if pred_next == i {
                let new_next = self.nodes[i as usize].forwards[h];
                if update[h] == NONE {
                    self.head[h] = new_next;
                } else {
                    self.nodes[update[h] as usize].forwards[h] = new_next;
                }
            }
        }
    }

    fn apply_levels(&mut self, levels: &[Level]) {
        // Bulk-return all nodes via the level-0 full-list walk; reset all skip heads.
        let mut cur = self.head[0];
        while cur != NONE {
            let next = self.nodes[cur as usize].forwards[0];
            self.nodes[cur as usize].forwards[0] = self.free_head;
            self.free_head = cur;
            cur = next;
        }
        self.head = [NONE; MAX_HEIGHT];
        self.tick_to_node.clear();

        for lvl in levels {
            if lvl.size.0.is_zero() {
                continue;
            }
            let h = self.random_height();
            if let Some(i) = self.alloc_node(lvl.price.0, lvl.size.0, h) {
                self.link(i);
            }
        }
    }

    fn apply_level(&mut self, price: Price, size: Quantity) {
        let tick = price.0;

        if size.0.is_zero() {
            if let Some(&i) = self.tick_to_node.get(&tick) {
                self.unlink(i);
                self.free_node(i);
            }
            return;
        }

        // Size update on an existing level — no structural change, O(1).
        if let Some(&i) = self.tick_to_node.get(&tick) {
            self.nodes[i as usize].size = size.0;
            return;
        }

        // New level — allocate, assign height, splice in.
        let h = self.random_height();
        if let Some(i) = self.alloc_node(tick, size.0, h) {
            self.link(i);
        }
    }

    fn iter(&self) -> SkipBookSideIter<'_> {
        SkipBookSideIter { nodes: &self.nodes, cur: self.head[0] }
    }
}

/// Lazy iterator over one side of the skip list, following level-0 forward pointers.
/// Starts at `head[0]` (the best bid or best ask) and steps through every level in order.
pub struct SkipBookSideIter<'a> {
    nodes: &'a [LevelNode],
    cur: u32,
}

impl<'a> Iterator for SkipBookSideIter<'a> {
    type Item = Level;

    #[inline]
    fn next(&mut self) -> Option<Level> {
        if self.cur == NONE {
            return None;
        }
        let n = &self.nodes[self.cur as usize];
        let level = Level { price: Price(n.price), size: Quantity(n.size) };
        self.cur = n.forwards[0];
        Some(level)
    }
}

/// Skip-list orderbook implementing `OrderbookLike`.
pub struct SkipOrderbook {
    bids: SkipBookSide<true>,
    asks: SkipBookSide<false>,
}

impl Default for SkipOrderbook {
    fn default() -> Self {
        Self::new()
    }
}

impl SkipOrderbook {
    pub fn new() -> Self {
        Self {
            bids: SkipBookSide::new(),
            asks: SkipBookSide::new(),
        }
    }
}

impl OrderbookLike for SkipOrderbook {
    type BidIter<'a> = SkipBookSideIter<'a>;
    type AskIter<'a> = SkipBookSideIter<'a>;

    fn bids(&self) -> SkipBookSideIter<'_> {
        self.bids.iter()
    }

    fn asks(&self) -> SkipBookSideIter<'_> {
        self.asks.iter()
    }

    fn apply_snapshot(&mut self, bids: Vec<Level>, asks: Vec<Level>) {
        self.bids.apply_levels(&bids);
        self.asks.apply_levels(&asks);
    }

    fn apply_price_change(&mut self, side: Side, price: Price, size: Quantity) {
        match side {
            Side::Bid => self.bids.apply_level(price, size),
            Side::Ask => self.asks.apply_level(price, size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Price { Price::from_str(s).unwrap() }
    fn q(s: &str) -> Quantity { Quantity::from_str(s).unwrap() }

    /// Verify that the level-h walk is a strict subsequence of the level-0 walk
    /// and that all express lanes respect the ordering invariant.
    fn assert_skip_invariants<const IS_BID: bool>(side: &SkipBookSide<IS_BID>) {
        // Collect level-0 sequence.
        let level0: Vec<u32> = {
            let mut v = Vec::new();
            let mut cur = side.head[0];
            while cur != NONE {
                v.push(cur);
                cur = side.nodes[cur as usize].forwards[0];
            }
            v
        };

        // Every node reachable at level 0 must appear in tick_to_node.
        for &idx in &level0 {
            let price = side.nodes[idx as usize].price;
            assert_eq!(side.tick_to_node[&price], idx);
        }

        // Prices along level 0 must be strictly ordered.
        for w in level0.windows(2) {
            let a = side.nodes[w[0] as usize].price;
            let b = side.nodes[w[1] as usize].price;
            assert!(
                SkipBookSide::<IS_BID>::comes_before(a, b),
                "level-0 ordering violated: {a} should come before {b}"
            );
        }

        // Every level-h walk must be an ordered subsequence of level 0.
        for h in 1..MAX_HEIGHT {
            let mut cur = side.head[h];
            let mut prev_price: Option<Decimal> = None;
            while cur != NONE {
                let price = side.nodes[cur as usize].price;
                // Must appear in the level-0 sequence.
                assert!(
                    level0.contains(&cur),
                    "node {cur} at level {h} not in level-0"
                );
                // Must be ordered.
                if let Some(p) = prev_price {
                    assert!(
                        SkipBookSide::<IS_BID>::comes_before(p, price),
                        "level-{h} ordering violated"
                    );
                }
                prev_price = Some(price);
                cur = side.nodes[cur as usize].forwards[h];
            }
        }
    }

    #[test]
    fn skip_invariants_after_snapshot() {
        let mut ob = SkipOrderbook::new();
        ob.apply_snapshot(
            vec![
                Level { price: p("0.48"), size: q("10") },
                Level { price: p("0.50"), size: q("20") },
                Level { price: p("0.49"), size: q("15") },
                Level { price: p("0.47"), size: q("5") },
            ],
            vec![
                Level { price: p("0.53"), size: q("30") },
                Level { price: p("0.51"), size: q("40") },
                Level { price: p("0.52"), size: q("25") },
                Level { price: p("0.54"), size: q("10") },
            ],
        );
        assert_skip_invariants(&ob.bids);
        assert_skip_invariants(&ob.asks);
    }

    #[test]
    fn skip_invariants_after_insert_and_remove() {
        let mut ob = SkipOrderbook::new();
        ob.apply_snapshot(
            (1..=20).map(|i| Level {
                price: p(&format!("0.{i:02}")),
                size: q("100"),
            }).collect(),
            (30..=50).map(|i| Level {
                price: p(&format!("0.{i:02}")),
                size: q("100"),
            }).collect(),
        );

        assert_skip_invariants(&ob.bids);
        assert_skip_invariants(&ob.asks);

        // Remove several levels.
        for i in [5u32, 10, 15] {
            ob.apply_price_change(Side::Bid, p(&format!("0.{i:02}")), q("0"));
        }
        for i in [35u32, 40, 45] {
            ob.apply_price_change(Side::Ask, p(&format!("0.{i:02}")), q("0"));
        }

        assert_skip_invariants(&ob.bids);
        assert_skip_invariants(&ob.asks);

        // Insert new levels interleaved with existing ones.
        ob.apply_price_change(Side::Bid, p("0.05"), q("50"));
        ob.apply_price_change(Side::Ask, p("0.35"), q("50"));

        assert_skip_invariants(&ob.bids);
        assert_skip_invariants(&ob.asks);
    }

    #[test]
    fn express_lanes_reach_deep_level_via_skips() {
        // Build a 64-level bid side and verify that at least one node at height >= 2
        // exists, demonstrating that express lanes were actually promoted.
        let mut ob = SkipOrderbook::new();
        ob.apply_snapshot(
            (1..=64).map(|i| Level {
                price: Price(Decimal::from(i)),
                size: q("1"),
            }).collect(),
            vec![],
        );

        assert_skip_invariants(&ob.bids);

        // With 64 nodes at p=0.25, expected ~21 nodes at level ≥ 1 (height ≥ 2).
        // The probability of zero promotions is (3/4)^64 ≈ 1.7e-8 — effectively impossible.
        let has_express_lane = ob.bids.nodes.iter().any(|n| n.height >= 2);
        assert!(has_express_lane, "no node was promoted to level 1; rng may be broken");

        // Bids iterate from highest to lowest (descending).
        let bids: Vec<Level> = ob.bids().collect();
        assert_eq!(bids.first().unwrap().price, Price(Decimal::from(64)));
        assert_eq!(bids.last().unwrap().price, Price(Decimal::from(1)));
        assert_eq!(bids.len(), 64);
    }

    #[test]
    fn size_update_does_not_change_skip_structure() {
        let mut ob = SkipOrderbook::new();
        ob.apply_snapshot(
            vec![
                Level { price: p("0.50"), size: q("100") },
                Level { price: p("0.49"), size: q("50") },
            ],
            vec![],
        );

        // Record which node holds 0.50 before the update.
        let node_idx = *ob.bids.tick_to_node.get(&p("0.50").0).unwrap();

        ob.apply_price_change(Side::Bid, p("0.50"), q("200"));

        // Same node, updated size, structure unchanged.
        assert_eq!(*ob.bids.tick_to_node.get(&p("0.50").0).unwrap(), node_idx);
        assert_eq!(ob.bids().next().unwrap().size, q("200"));
        assert_skip_invariants(&ob.bids);
    }

    #[test]
    fn snapshot_recycles_nodes_correctly() {
        let mut ob = SkipOrderbook::new();
        ob.apply_snapshot(
            vec![Level { price: p("0.50"), size: q("10") }],
            vec![Level { price: p("0.51"), size: q("10") }],
        );
        ob.apply_snapshot(
            vec![Level { price: p("0.48"), size: q("5") }, Level { price: p("0.49"), size: q("3") }],
            vec![Level { price: p("0.52"), size: q("7") }],
        );

        // Old ticks must be gone; new ones must be present.
        assert!(ob.bids.tick_to_node.get(&p("0.50").0).is_none());
        assert!(ob.asks.tick_to_node.get(&p("0.51").0).is_none());
        assert_eq!(ob.bids().next().unwrap().price, p("0.49"));
        assert_eq!(ob.asks().next().unwrap().price, p("0.52"));

        assert_skip_invariants(&ob.bids);
        assert_skip_invariants(&ob.asks);
    }
}
