use ws_session::basic_orderbook::{Price, Quantity};

#[test]
fn price_parsing_and_display() {
    // Standard round-trips via rust_decimal's natural representation.
    assert_eq!(Price::from_str("0.52").unwrap().to_string(), "0.52");
    assert_eq!(Price::from_str("1").unwrap().to_string(), "1");
    assert_eq!(Price::from_str("0.5").unwrap().to_string(), "0.5");
    assert_eq!(Price::from_str("0.123456").unwrap().to_string(), "0.123456");
    assert_eq!(Price::from_str("2.1").unwrap().to_string(), "2.1");
    // rust_decimal preserves trailing zeros present in the source string.
    assert_eq!(Price::from_str("2.10").unwrap().to_string(), "2.10");
}

#[test]
fn quantity_parsing_and_display() {
    assert_eq!(Quantity::from_str("10").unwrap().to_string(), "10");
    assert_eq!(Quantity::from_str("10.5").unwrap().to_string(), "10.5");
    assert_eq!(Quantity::from_str("0.123456").unwrap().to_string(), "0.123456");
}

#[test]
fn invalid_inputs_return_none() {
    assert!(Price::from_str("abc").is_none());
    assert!(Price::from_str("1.2.3").is_none());
    assert!(Quantity::from_str("abc").is_none());
    assert!(Quantity::from_str("1.2.3").is_none());
}
