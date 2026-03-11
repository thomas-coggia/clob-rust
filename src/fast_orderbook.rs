//! Intrusive-list-based orderbook: pool-allocated nodes, O(1) lookup by price tick,
//! O(depth) insert/remove, cache-free — levels are traversed lazily via an iterator.
//! IS_BID is a const generic: the comparison operator is resolved at compile time.

use crate::basic_orderbook::{Level, OrderbookLike, Price, Quantity, Side};
use rust_decimal::Decimal;
use std::collections::HashMap;

const NONE: u32 = u32::MAX;

/// One node in the intrusive price-ordered list.
/// Holds both the level data and the list links; when free, `next` is the free-list link.
#[repr(C)]
struct LevelNode {
    price: Decimal,
    size: Decimal,
    next: u32,
    prev: u32,
}

/// Single side (bids or asks): pool of nodes + intrusive doubly-linked list + tick→node map.
/// IS_BID = true  → descending order (highest price first).
/// IS_BID = false → ascending order  (lowest price first).
struct FastBookSide<const IS_BID: bool> {
    nodes: Vec<LevelNode>,
    free_head: u32,
    head: u32,
    tick_to_node: HashMap<Decimal, u32>,
}

impl<const IS_BID: bool> FastBookSide<IS_BID> {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            free_head: NONE,
            head: NONE,
            tick_to_node: HashMap::with_capacity(64),
        }
    }

    /// Returns true when `a` should appear before `b` in the list.
    /// Resolved at compile time; no runtime branch.
    #[inline(always)]
    fn comes_before(a: Decimal, b: Decimal) -> bool {
        if IS_BID { a > b } else { a < b }
    }

    #[inline]
    fn alloc_node(&mut self, price: Decimal, size: Decimal) -> Option<u32> {
        if self.free_head != NONE {
            let i = self.free_head;
            let node = &mut self.nodes[i as usize];
            self.free_head = node.next;
            node.price = price;
            node.size = size;
            node.next = NONE;
            node.prev = NONE;
            Some(i)
        } else {
            let i = self.nodes.len() as u32;
            if i == NONE {
                return None;
            }
            self.nodes.push(LevelNode { price, size, next: NONE, prev: NONE });
            Some(i)
        }
    }

    #[inline]
    fn free_node(&mut self, i: u32) {
        let tick = self.nodes[i as usize].price;
        self.tick_to_node.remove(&tick);
        self.nodes[i as usize].next = self.free_head;
        self.free_head = i;
    }

    fn unlink(&mut self, i: u32) {
        let prev = self.nodes[i as usize].prev;
        let next = self.nodes[i as usize].next;
        if prev != NONE {
            self.nodes[prev as usize].next = next;
        } else {
            self.head = next;
        }
        if next != NONE {
            self.nodes[next as usize].prev = prev;
        }
    }

    /// Insert node `i` in price order. Caller must have already allocated and filled `i`.
    fn link(&mut self, i: u32) {
        let tick = self.nodes[i as usize].price;
        self.tick_to_node.insert(tick, i);

        if self.head == NONE {
            self.nodes[i as usize].prev = NONE;
            self.nodes[i as usize].next = NONE;
            self.head = i;
            return;
        }

        let mut cur = self.head;
        loop {
            let cur_tick = self.nodes[cur as usize].price;
            if Self::comes_before(tick, cur_tick) {
                let prev = self.nodes[cur as usize].prev;
                self.nodes[i as usize].prev = prev;
                self.nodes[i as usize].next = cur;
                self.nodes[cur as usize].prev = i;
                if prev != NONE {
                    self.nodes[prev as usize].next = i;
                } else {
                    self.head = i;
                }
                return;
            }
            let next = self.nodes[cur as usize].next;
            if next == NONE {
                self.nodes[cur as usize].next = i;
                self.nodes[i as usize].prev = cur;
                self.nodes[i as usize].next = NONE;
                return;
            }
            cur = next;
        }
    }

    fn apply_levels(&mut self, levels: &[Level]) {
        // Bulk-return all nodes to the free list; clear the map once instead of per-node.
        let mut cur = self.head;
        while cur != NONE {
            let next = self.nodes[cur as usize].next;
            self.nodes[cur as usize].next = self.free_head;
            self.free_head = cur;
            cur = next;
        }
        self.head = NONE;
        self.tick_to_node.clear();

        for lvl in levels {
            if lvl.size.0.is_zero() {
                continue;
            }
            if let Some(i) = self.alloc_node(lvl.price.0, lvl.size.0) {
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

        if let Some(&i) = self.tick_to_node.get(&tick) {
            self.nodes[i as usize].size = size.0;
            return;
        }

        if let Some(i) = self.alloc_node(tick, size.0) {
            self.link(i);
        }
    }

    fn iter(&self) -> FastBookSideIter<'_> {
        FastBookSideIter { nodes: &self.nodes, cur: self.head }
    }
}

/// Lazy iterator over one side of the book, following intrusive list links.
/// No buffer; each `next()` reads directly from the node pool.
pub struct FastBookSideIter<'a> {
    nodes: &'a [LevelNode],
    cur: u32,
}

impl<'a> Iterator for FastBookSideIter<'a> {
    type Item = Level;

    #[inline]
    fn next(&mut self) -> Option<Level> {
        if self.cur == NONE {
            return None;
        }
        let n = &self.nodes[self.cur as usize];
        let level = Level { price: Price(n.price), size: Quantity(n.size) };
        self.cur = n.next;
        Some(level)
    }
}

/// Fast orderbook: intrusive-list internals implementing `OrderbookLike`.
pub struct FastOrderbook {
    bids: FastBookSide<true>,
    asks: FastBookSide<false>,
}

impl Default for FastOrderbook {
    fn default() -> Self {
        Self::new()
    }
}

impl FastOrderbook {
    pub fn new() -> Self {
        Self {
            bids: FastBookSide::new(),
            asks: FastBookSide::new(),
        }
    }
}

impl OrderbookLike for FastOrderbook {
    type BidIter<'a> = FastBookSideIter<'a>;
    type AskIter<'a> = FastBookSideIter<'a>;

    fn bids(&self) -> FastBookSideIter<'_> {
        self.bids.iter()
    }

    fn asks(&self) -> FastBookSideIter<'_> {
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
