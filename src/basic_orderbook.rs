use std::collections::HashMap;
use std::fmt;

use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Price(pub Decimal);

impl Price {
    pub fn from_str(s: &str) -> Option<Self> {
        s.parse().ok().map(Price)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Quantity(pub Decimal);

impl Quantity {
    pub fn from_str(s: &str) -> Option<Self> {
        s.parse().ok().map(Quantity)
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub price: Price,
    pub size: Quantity,
}

/// Common interface for any orderbook implementation.
///
/// Only `bids` and `asks` (plus the two mutation methods) are required.
/// Everything else — best level, spread — has a default impl derived from the iterators.
pub trait OrderbookLike: Default {
    type BidIter<'a>: Iterator<Item = Level>
    where
        Self: 'a;
    type AskIter<'a>: Iterator<Item = Level>
    where
        Self: 'a;

    fn apply_snapshot(&mut self, bids: Vec<Level>, asks: Vec<Level>);
    fn apply_price_change(&mut self, side: Side, price: Price, size: Quantity);

    /// Levels on the bid side in descending price order.
    fn bids(&self) -> Self::BidIter<'_>;
    /// Levels on the ask side in ascending price order.
    fn asks(&self) -> Self::AskIter<'_>;

    fn best_bid(&self) -> Option<Level> {
        self.bids().next()
    }

    fn best_ask(&self) -> Option<Level> {
        self.asks().next()
    }

    fn spread(&self) -> Option<Price> {
        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;
        if best_ask.price.0 > best_bid.price.0 {
            Some(Price(best_ask.price.0 - best_bid.price.0))
        } else {
            Some(Price(Decimal::ZERO))
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct BookSide {
    levels: Vec<Level>,
}

impl BookSide {
    fn from_levels(mut levels: Vec<Level>, side: Side) -> Self {
        match side {
            Side::Bid => levels.sort_by(|a, b| b.price.cmp(&a.price)),
            Side::Ask => levels.sort_by(|a, b| a.price.cmp(&b.price)),
        }
        Self { levels }
    }

    fn apply_level(&mut self, side: Side, price: Price, size: Quantity) {
        if size.0.is_zero() {
            self.levels.retain(|lvl| lvl.price != price);
            return;
        }

        match self.levels.iter_mut().find(|lvl| lvl.price == price) {
            Some(level) => level.size = size,
            None => self.levels.push(Level { price, size }),
        }

        match side {
            Side::Bid => self.levels.sort_by(|a, b| b.price.cmp(&a.price)),
            Side::Ask => self.levels.sort_by(|a, b| a.price.cmp(&b.price)),
        }
    }
}

/// Reference implementation: simple Vec-based, used for testing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BasicOrderbook {
    bids: BookSide,
    asks: BookSide,
}

impl OrderbookLike for BasicOrderbook {
    type BidIter<'a> = std::iter::Copied<std::slice::Iter<'a, Level>>;
    type AskIter<'a> = std::iter::Copied<std::slice::Iter<'a, Level>>;

    fn bids(&self) -> Self::BidIter<'_> {
        self.bids.levels.iter().copied()
    }

    fn asks(&self) -> Self::AskIter<'_> {
        self.asks.levels.iter().copied()
    }

    fn apply_snapshot(&mut self, bids: Vec<Level>, asks: Vec<Level>) {
        self.bids = BookSide::from_levels(bids, Side::Bid);
        self.asks = BookSide::from_levels(asks, Side::Ask);
    }

    fn apply_price_change(&mut self, side: Side, price: Price, size: Quantity) {
        match side {
            Side::Bid => self.bids.apply_level(Side::Bid, price, size),
            Side::Ask => self.asks.apply_level(Side::Ask, price, size),
        }
    }
}

#[derive(Debug)]
pub struct Orderbooks<B> {
    books: HashMap<String, B>,
}

impl<B: Default> Default for Orderbooks<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: Default> Orderbooks<B> {
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
        }
    }

    pub fn book_mut(&mut self, asset_id: &str) -> &mut B {
        self.books.entry(asset_id.to_string()).or_default()
    }

    pub fn get(&self, asset_id: &str) -> Option<&B> {
        self.books.get(asset_id)
    }

    pub fn get_mut(&mut self, asset_id: &str) -> Option<&mut B> {
        self.books.get_mut(asset_id)
    }

    pub fn assets(&self) -> impl Iterator<Item = (&String, &B)> {
        self.books.iter()
    }
}
