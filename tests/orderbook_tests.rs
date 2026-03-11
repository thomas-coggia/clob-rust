use rust_decimal::Decimal;
use ws_session::basic_orderbook::{BasicOrderbook, Level, OrderbookLike, Price, Quantity, Side};
use ws_session::{FastOrderbook, SkipOrderbook};

fn p(s: &str) -> Price {
    Price::from_str(s).expect("valid price")
}

fn q(s: &str) -> Quantity {
    Quantity::from_str(s).expect("valid quantity")
}

fn cumul<'a>(iter: impl Iterator<Item = Level>) -> Vec<Quantity> {
    let mut acc = Decimal::ZERO;
    iter.map(|lvl| {
        acc += lvl.size.0;
        Quantity(acc)
    })
    .collect()
}

fn run_basic_orderbook_suite<O: OrderbookLike>() {
    // Snapshot sorts bids descending / asks ascending and computes cumulatives correctly.
    let mut ob = O::default();
    ob.apply_snapshot(
        vec![
            Level { price: p("0.48"), size: q("30") },
            Level { price: p("0.49"), size: q("20") },
        ],
        vec![
            Level { price: p("0.53"), size: q("60") },
            Level { price: p("0.52"), size: q("25") },
        ],
    );

    let bids: Vec<Level> = ob.bids().collect();
    assert_eq!(bids[0].price, p("0.49"));
    assert_eq!(bids[1].price, p("0.48"));

    let asks: Vec<Level> = ob.asks().collect();
    assert_eq!(asks[0].price, p("0.52"));
    assert_eq!(asks[1].price, p("0.53"));

    assert_eq!(cumul(ob.bids()), vec![q("20"), q("50")]);
    assert_eq!(cumul(ob.asks()), vec![q("25"), q("85")]);

    // Price changes: insert, update, remove.
    ob.apply_price_change(Side::Bid, p("0.50"), q("200"));
    assert_eq!(ob.bids().next().unwrap().price, p("0.50"));
    assert_eq!(ob.bids().next().unwrap().size, q("200"));

    ob.apply_price_change(Side::Ask, p("0.51"), q("60"));
    assert_eq!(ob.asks().next().unwrap().price, p("0.51"));
    assert_eq!(ob.asks().next().unwrap().size, q("60"));

    ob.apply_price_change(Side::Ask, p("0.53"), q("0"));
    assert!(ob.asks().all(|lvl| lvl.price != p("0.53")));

    // Spread and best levels.
    assert_eq!(ob.spread().unwrap(), p("0.01")); // 0.52 - 0.51 = 0.01
    assert_eq!(ob.best_bid().unwrap().price, p("0.50"));
    assert_eq!(ob.best_ask().unwrap().price, ob.asks().next().unwrap().price);
}

fn run_edge_case_orderbook_suite<O: OrderbookLike>() {
    // Empty snapshot → no best bid/ask, no spread.
    let mut ob = O::default();
    ob.apply_snapshot(Vec::new(), Vec::new());
    assert!(ob.best_bid().is_none());
    assert!(ob.best_ask().is_none());
    assert!(ob.spread().is_none());

    // Only bids → best bid present, no best ask, no spread.
    ob.apply_snapshot(
        vec![
            Level { price: p("0.40"), size: q("10") },
            Level { price: p("0.39"), size: q("5") },
        ],
        Vec::new(),
    );
    assert_eq!(ob.best_bid().unwrap().price, p("0.40"));
    assert!(ob.best_ask().is_none());
    assert!(ob.spread().is_none());

    // Only asks → best ask present, no best bid, no spread.
    ob.apply_snapshot(
        Vec::new(),
        vec![
            Level { price: p("0.60"), size: q("7") },
            Level { price: p("0.61"), size: q("3") },
        ],
    );
    assert!(ob.best_bid().is_none());
    assert_eq!(ob.best_ask().unwrap().price, p("0.60"));
    assert!(ob.spread().is_none());

    // Removing last levels via price_change with size 0 clears the side.
    ob.apply_snapshot(
        vec![Level { price: p("0.45"), size: q("100") }],
        vec![Level { price: p("0.55"), size: q("200") }],
    );
    ob.apply_price_change(Side::Bid, p("0.45"), q("0"));
    assert!(ob.best_bid().is_none());
    ob.apply_price_change(Side::Ask, p("0.55"), q("0"));
    assert!(ob.best_ask().is_none());
    assert!(ob.spread().is_none());
}

#[test]
fn reference_orderbook_passes_shared_suites() {
    run_basic_orderbook_suite::<BasicOrderbook>();
    run_edge_case_orderbook_suite::<BasicOrderbook>();
}

#[test]
fn fast_orderbook_passes_shared_suites() {
    run_basic_orderbook_suite::<FastOrderbook>();
    run_edge_case_orderbook_suite::<FastOrderbook>();
}

#[test]
fn skip_orderbook_passes_shared_suites() {
    run_basic_orderbook_suite::<SkipOrderbook>();
    run_edge_case_orderbook_suite::<SkipOrderbook>();
}
