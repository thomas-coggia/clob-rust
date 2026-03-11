use crate::basic_orderbook::{Level, OrderbookLike, Orderbooks, Price, Quantity, Side};
use rust_decimal::Decimal;
use serde::{Serialize};
use serde_json::Value;
use thiserror::Error;
use std::collections::HashMap;
use log::{info, warn};
use tokio::sync::broadcast;

#[derive(Debug, Error)]
pub enum SessionObserverError {
    #[error("observer error: {0}")]
    Other(String),
}

pub trait SessionObserver: Send {
    fn on_connected(&mut self) {}
    fn on_disconnected(&mut self) {}
    fn on_json_message(&mut self, _value: Value) {}
    fn on_error(&mut self, _error: SessionObserverError) {}
}

pub struct OrderbookObserver<B: OrderbookLike + Default + Send> {
    books: Orderbooks<B>,
    labels: HashMap<String, String>,
    notifier: Option<broadcast::Sender<TopBookSnapshot>>,
    verbose: bool,
}

pub struct AssetConfig {
    pub id: String,
    pub label: String,
}

pub struct ObserverConfig {
    pub assets: Vec<AssetConfig>,
    /// Optional channel used to publish formatted order book snapshots.
    pub notifier: Option<broadcast::Sender<TopBookSnapshot>>,
    /// When true, pretty-print books to stdout on changes.
    pub verbose: bool,
}

const DISPLAY_DEPTH: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub struct BookLevel {
    pub price: String,
    pub size: String,
    pub cumulative: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetBook {
    pub asset_id: String,
    pub label: String,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub spread: Option<String>,
    pub best_bid: Option<String>,
    pub best_ask: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopBookSnapshot {
    pub assets: Vec<AssetBook>,
}

impl<B: OrderbookLike + Default + Send> OrderbookObserver<B> {
    pub fn new(config: ObserverConfig) -> Self {
        let mut labels = HashMap::new();
        for asset in config.assets {
            labels.insert(asset.id, asset.label);
        }

        Self {
            books: Orderbooks::new(),
            labels,
            notifier: config.notifier,
            verbose: config.verbose,
        }
    }

    fn handle_single_message(&mut self, value: &Value) {
        let event_type = match value.get("event_type").and_then(Value::as_str) {
            Some(t) => t,
            None => return,
        };

        match event_type {
            "book" => self.handle_book(value),
            "price_change" => self.handle_price_change(value),
            _ => {}
        }
    }

    fn handle_book(&mut self, value: &Value) {
        let asset_id = match value.get("asset_id").and_then(Value::as_str) {
            Some(id) => id,
            None => return,
        };

        if !self.labels.contains_key(asset_id) {
            return;
        }

        let bids = match value.get("bids").and_then(Value::as_array) {
            Some(b) => b,
            None => return,
        };
        let asks = match value.get("asks").and_then(Value::as_array) {
            Some(a) => a,
            None => return,
        };

        let parse_levels = |entries: &[Value]| -> Option<Vec<Level>> {
            let mut out = Vec::with_capacity(entries.len());
            for e in entries {
                let price_str = e.get("price")?.as_str()?;
                let size_str = e.get("size")?.as_str()?;
                let price = Price::from_str(price_str)?;
                let size = Quantity::from_str(size_str)?;
                out.push(Level { price, size });
            }
            Some(out)
        };

        let bids_levels = match parse_levels(bids) {
            Some(b) => b,
            None => return,
        };
        let asks_levels = match parse_levels(asks) {
            Some(a) => a,
            None => return,
        };

        let book = self.books.book_mut(asset_id);

        // Capture previous top-of-book view (up to DISPLAY_DEPTH levels per side).
        let prev_top_bids: Vec<Level> = book.bids().take(DISPLAY_DEPTH).collect();
        let prev_top_asks: Vec<Level> = book.asks().take(DISPLAY_DEPTH).collect();

        book.apply_snapshot(bids_levels, asks_levels);

        let new_top_bids: Vec<Level> = book.bids().take(DISPLAY_DEPTH).collect();
        let new_top_asks: Vec<Level> = book.asks().take(DISPLAY_DEPTH).collect();

        if prev_top_bids != new_top_bids || prev_top_asks != new_top_asks {
            self.publish_and_print_books();
        }
    }

    fn handle_price_change(&mut self, value: &Value) {
        let changes = match value.get("price_changes").and_then(Value::as_array) {
            Some(c) => c,
            None => return,
        };

        let mut should_print = false;

        for change in changes {
            let asset_id = match change.get("asset_id").and_then(Value::as_str) {
                Some(id) => id,
                None => continue,
            };

            if !self.labels.contains_key(asset_id) {
                continue;
            }
            let price_str = match change.get("price").and_then(Value::as_str) {
                Some(p) => p,
                None => continue,
            };
            let size_str = match change.get("size").and_then(Value::as_str) {
                Some(s) => s,
                None => continue,
            };
            let side_str = match change.get("side").and_then(Value::as_str) {
                Some(s) => s,
                None => continue,
            };

            let price = match Price::from_str(price_str) {
                Some(p) => p,
                None => continue,
            };
            let size = match Quantity::from_str(size_str) {
                Some(q) => q,
                None => continue,
            };

            let side = match side_str {
                "BUY" => Side::Bid,
                "SELL" => Side::Ask,
                _ => continue,
            };

            let Some(book) = self.books.get_mut(asset_id) else {
                warn!(
                    "protocol violation: received price_change for asset `{asset_id}` before any book snapshot"
                );
                continue;
            };

            // Determine whether this change can affect the visible top DISPLAY_DEPTH levels.
            let old_index = match side {
                Side::Bid => book.bids().position(|lvl| lvl.price == price),
                Side::Ask => book.asks().position(|lvl| lvl.price == price),
            };
            let old_in_top = old_index.map_or(false, |idx| idx < DISPLAY_DEPTH);

            // Apply the change (this may insert, update, or remove a level and re-sort).
            book.apply_price_change(side, price, size);

            let new_index = match side {
                Side::Bid => book.bids().position(|lvl| lvl.price == price),
                Side::Ask => book.asks().position(|lvl| lvl.price == price),
            };
            let new_in_top = new_index.map_or(false, |idx| idx < DISPLAY_DEPTH);

            if old_in_top || new_in_top {
                should_print = true;
            }
        }

        if should_print {
            self.publish_and_print_books();
        }
    }

    fn format_books_text(&self) -> String {
        let mut out = String::new();
        for (asset_id, label) in &self.labels {
            let Some(book) = self.books.get(asset_id) else {
                continue;
            };
            out.push_str(&format!("=== {label} Orderbook ===\n"));
            out.push_str("        BIDS                          ASKS\n");
            out.push_str(&format!(
                "  {:>7} | {:>15} | {:>15}      {:>7} | {:>15} | {:>15}\n",
                "Price", "Size", "Cumul.", "Price", "Size", "Cumul."
            ));

            let bids: Vec<(Level, Quantity)> = {
                let mut cumul = Decimal::ZERO;
                book.bids().take(DISPLAY_DEPTH).map(|lvl| {
                    cumul += lvl.size.0;
                    (lvl, Quantity(cumul))
                }).collect()
            };
            let asks: Vec<(Level, Quantity)> = {
                let mut cumul = Decimal::ZERO;
                book.asks().take(DISPLAY_DEPTH).map(|lvl| {
                    cumul += lvl.size.0;
                    (lvl, Quantity(cumul))
                }).collect()
            };
            let rows = bids.len().max(asks.len());
            for i in 0..rows {
                let bid_str = if let Some((lvl, cumul)) = bids.get(i) {
                    format!(
                        "  {:>7} | {:>15} | {:>15}",
                        lvl.price.to_string(),
                        lvl.size.to_string(),
                        cumul.to_string()
                    )
                } else {
                    format!("  {:>7} | {:>15} | {:>15}", "", "", "")
                };

                let ask_str = if let Some((lvl, cumul)) = asks.get(i) {
                    format!(
                        "      {:>7} | {:>15} | {:>15}",
                        lvl.price.to_string(),
                        lvl.size.to_string(),
                        cumul.to_string()
                    )
                } else {
                    "".to_string()
                };

                out.push_str(&format!("{bid_str}{ask_str}\n"));
            }

            if let Some(spread) = book.spread() {
                if let (Some(best_bid), Some(best_ask)) = (book.best_bid(), book.best_ask()) {
                    out.push_str(&format!(
                        "  Spread: {} | Best Bid: {} | Best Ask: {}\n",
                        spread.to_string(),
                        best_bid.price.to_string(),
                        best_ask.price.to_string()
                    ));
                }
            }
            out.push('\n');
        }
        out
    }

    fn build_top_snapshot(&self) -> TopBookSnapshot {
        let mut assets = Vec::new();

        for (asset_id, label) in &self.labels {
            let Some(book) = self.books.get(asset_id) else {
                continue;
            };

            let bid_levels: Vec<BookLevel> = {
                let mut cumul = Decimal::ZERO;
                book.bids().take(DISPLAY_DEPTH).map(|lvl| {
                    cumul += lvl.size.0;
                    BookLevel {
                        price: lvl.price.to_string(),
                        size: lvl.size.to_string(),
                        cumulative: Quantity(cumul).to_string(),
                    }
                }).collect()
            };

            let ask_levels: Vec<BookLevel> = {
                let mut cumul = Decimal::ZERO;
                book.asks().take(DISPLAY_DEPTH).map(|lvl| {
                    cumul += lvl.size.0;
                    BookLevel {
                        price: lvl.price.to_string(),
                        size: lvl.size.to_string(),
                        cumulative: Quantity(cumul).to_string(),
                    }
                }).collect()
            };

            let (spread_str, best_bid_str, best_ask_str) = if let Some(spread) = book.spread() {
                let best_bid = book.best_bid();
                let best_ask = book.best_ask();
                (
                    Some(spread.to_string()),
                    best_bid.map(|lvl| lvl.price.to_string()),
                    best_ask.map(|lvl| lvl.price.to_string()),
                )
            } else {
                (None, None, None)
            };

            assets.push(AssetBook {
                asset_id: asset_id.clone(),
                label: label.clone(),
                bids: bid_levels,
                asks: ask_levels,
                spread: spread_str,
                best_bid: best_bid_str,
                best_ask: best_ask_str,
            });
        }

        TopBookSnapshot { assets }
    }

    fn publish_and_print_books(&self) {
        if self.verbose {
            let text = self.format_books_text();
            println!("{text}");
        }
        let snapshot = self.build_top_snapshot();
        if let Some(sender) = &self.notifier {
            // Ignore send errors (e.g. no active receivers).
            let _ = sender.send(snapshot);
        }
    }
}

impl<B: OrderbookLike + Default + Send> SessionObserver for OrderbookObserver<B> {
    fn on_connected(&mut self) {
        info!("connected to Polymarket websocket");
    }

    fn on_disconnected(&mut self) {
        info!("disconnected from Polymarket websocket");
    }

    fn on_json_message(&mut self, value: Value) {
        match value {
            Value::Array(values) => {
                for v in &values {
                    self.handle_single_message(v);
                }
            }
            other => {
                self.handle_single_message(&other);
            }
        }
    }

    fn on_error(&mut self, error: SessionObserverError) {
        warn!("observer error: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basic_orderbook::BasicOrderbook;
    use serde_json::json;

    const YES_ASSET_ID: &str = "YES_ASSET";
    const NO_ASSET_ID: &str = "NO_ASSET";

    fn p(s: &str) -> Price {
        Price::from_str(s).expect("valid price")
    }

    fn q(s: &str) -> Quantity {
        Quantity::from_str(s).expect("valid quantity")
    }

    #[test]
    fn book_snapshot_is_applied_for_yes_asset() {
        let mut obs = OrderbookObserver::<BasicOrderbook>::new(ObserverConfig {
            assets: vec![
                AssetConfig {
                    id: YES_ASSET_ID.to_string(),
                    label: "YES".to_string(),
                },
                AssetConfig {
                    id: NO_ASSET_ID.to_string(),
                    label: "NO".to_string(),
                },
            ],
            notifier: None,
            verbose: true,
        });

        let msg = json!({
            "event_type": "book",
            "asset_id": YES_ASSET_ID,
            "market": "condition",
            "bids": [
                { "price": "0.48", "size": "30" },
                { "price": "0.49", "size": "20" }
            ],
            "asks": [
                { "price": "0.52", "size": "25" },
                { "price": "0.53", "size": "60" }
            ],
            "timestamp": "123456789000",
            "hash": "0x0"
        });

        obs.on_json_message(msg);

        let book = obs.books.get(YES_ASSET_ID).expect("book for YES asset");
        assert_eq!(book.bids().count(), 2);
        assert_eq!(book.asks().count(), 2);
        assert_eq!(book.bids().next().unwrap().price, p("0.49"));
        assert_eq!(book.asks().next().unwrap().price, p("0.52"));
    }

    #[test]
    fn price_change_updates_existing_levels_after_snapshot() {
        let mut obs = OrderbookObserver::<BasicOrderbook>::new(ObserverConfig {
            assets: vec![
                AssetConfig {
                    id: YES_ASSET_ID.to_string(),
                    label: "YES".to_string(),
                },
            ],
            notifier: None,
            verbose: true,
        });

        let snapshot = json!({
            "event_type": "book",
            "asset_id": YES_ASSET_ID,
            "market": "condition",
            "bids": [
                { "price": "0.50", "size": "100" }
            ],
            "asks": [
                { "price": "0.52", "size": "80" }
            ],
            "timestamp": "1",
            "hash": "0x0"
        });
        obs.on_json_message(snapshot);

        let delta = json!({
            "event_type": "price_change",
            "market": "condition",
            "price_changes": [
                {
                    "asset_id": YES_ASSET_ID,
                    "price": "0.50",
                    "size": "200",
                    "side": "BUY"
                },
                {
                    "asset_id": YES_ASSET_ID,
                    "price": "0.52",
                    "size": "0",
                    "side": "SELL"
                }
            ],
            "timestamp": "2"
        });
        obs.on_json_message(delta);

        let book = obs.books.get(YES_ASSET_ID).expect("book for YES asset");
        assert_eq!(book.bids().count(), 1);
        assert_eq!(book.bids().next().unwrap().price, p("0.50"));
        assert_eq!(book.bids().next().unwrap().size, q("200"));
        // ask level at 0.52 should have been removed
        assert!(book.asks().next().is_none());
    }

    #[test]
    fn array_of_messages_is_processed_sequentially() {
        let mut obs = OrderbookObserver::<BasicOrderbook>::new(ObserverConfig {
            assets: vec![
                AssetConfig {
                    id: YES_ASSET_ID.to_string(),
                    label: "YES".to_string(),
                },
            ],
            notifier: None,
            verbose: true,
        });

        let msgs = json!([
            {
                "event_type": "book",
                "asset_id": YES_ASSET_ID,
                "market": "condition",
                "bids": [
                    { "price": "0.50", "size": "100" }
                ],
                "asks": [
                    { "price": "0.52", "size": "80" }
                ],
                "timestamp": "1",
                "hash": "0x0"
            },
            {
                "event_type": "price_change",
                "market": "condition",
                "price_changes": [
                    {
                        "asset_id": YES_ASSET_ID,
                        "price": "0.51",
                        "size": "60",
                        "side": "SELL"
                    }
                ],
                "timestamp": "2"
            }
        ]);

        obs.on_json_message(msgs);

        let book = obs.books.get(YES_ASSET_ID).expect("book for YES asset");
        assert_eq!(book.bids().count(), 1);
        assert_eq!(book.bids().next().unwrap().price, p("0.50"));
        assert_eq!(book.asks().count(), 2);
        assert_eq!(book.asks().next().unwrap().price, p("0.51"));
        assert_eq!(book.asks().next().unwrap().size, q("60"));
    }

    #[test]
    fn price_change_before_snapshot_does_not_create_book() {
        let mut obs = OrderbookObserver::<BasicOrderbook>::new(ObserverConfig {
            assets: vec![AssetConfig {
                id: YES_ASSET_ID.to_string(),
                label: "YES".to_string(),
            }],
            notifier: None,
            verbose: true,
        });

        let delta = json!({
            "event_type": "price_change",
            "market": "condition",
            "price_changes": [
                {
                    "asset_id": YES_ASSET_ID,
                    "price": "0.50",
                    "size": "200",
                    "side": "BUY"
                }
            ],
            "timestamp": "1"
        });

        obs.on_json_message(delta);

        // No snapshot was received for YES_ASSET_ID, so no book should exist.
        assert!(obs.books.get(YES_ASSET_ID).is_none());
    }
}


