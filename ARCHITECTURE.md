## Architecture Overview

This repository is a single Rust crate, `ws_session`, with three binaries (`live`, `capture`, `replay`) built on a shared core. The crate is organized by responsibilities rather than by binaries: each binary wires together the same core components in a slightly different way.

### High-level data flow

```text
LIVE (WebSocket → in-memory books → HTTP)
=========================================

  Polymarket WebSocket
            |
            v
   +------------------+       +---------------------------+
   |  session         | ----> |  OrderbookObserver       |
   |  (I/O + state)   |       |  (SessionObserver impl.) |
   +------------------+       +---------------------------+
            |                          |
     drives | STATE                    | mutates
            v                          v
        [state]                 [FastOrderbook]
                                         |
                                         v
                                   HTTP server
                                 (/ and /api/books)


CAPTURE (WebSocket → file)
==========================

  Polymarket WebSocket
            |
            v
   +------------------+       +---------------------------+
   |  session         | ----> |  CaptureObserver         |
   |  (I/O + state)   |       |  (SessionObserver impl.) |
   +------------------+       +---------------------------+
                                         |
                                         v
                                   capture.ndjson


REPLAY (file → in-memory books → stdout)
========================================

   capture.ndjson
          |
          v
   +------------------+       +---------------------------+
   |  replay binary   | ----> |  OrderbookObserver       |
   |  (read + parse)  |       |  (SessionObserver impl.) |
   +------------------+       +---------------------------+
                                       |
                                       v
                              [FastOrderbook]
                                       |
                                       v
                               stdout (text books)
```

### Component roles

- **Binaries**
  - **`live`**: runs a single-threaded WebSocket session against Polymarket, maintains in-memory order books via `OrderbookObserver`, and exposes a small HTTP server (`/` HTML dashboard, `/api/books` JSON).
  - **`capture`**: runs the same WebSocket session, but uses a capture-oriented observer that writes each incoming JSON message as NDJSON.
  - **`replay`**: reads NDJSON from disk and feeds it into `OrderbookObserver` line-by-line, as if it were live.

- **Core crate (`ws_session`)**
  - **`config`**: defines `AppConfig` (WebSocket URL, assets, optional CPU pinning) and builds the subscription payload shared by all binaries.
  - **`session`**: owns the async WebSocket connection, reconnection loop, and backoff; drives the pure `state` machine and delivers typed events to any `SessionObserver`.
  - **`state`**: a pure state machine that classifies incoming frames and emits `Effect`s (connect, disconnect, resubscribe, etc.), isolated from I/O.
  - **`observer`**: defines the `SessionObserver` trait and provides `OrderbookObserver` plus `TopBookSnapshot` for rendering and HTTP export.
  - **`basic_orderbook` / `fast_orderbook` / `skip_orderbook`**: three implementations of a shared `OrderbookLike` trait (see below).
  - **`pin`**: optional helper to pin the current thread to a specific CPU when `cpu` is configured.

External dependencies are kept at the edges (WebSocket client, HTTP server); everything between `session`, `state`, `observer`, and the orderbooks is designed to be deterministic and testable in isolation.

---

### Orderbook design

The orderbook implementations prioritize cache efficiency with a tradeoff on algorithmic complexity for deep-level insertions, while favoring quick iteration at the top of book. Deep-level insertions can be mitigated to O(log n) with a small improvement using the skip orderbook implementation.

#### Complexity comparison

| Implementation | Snapshot | Insert | Delete | Size update | Best level | Iter depth k |
|---------------|----------|--------|--------|-------------|------------|--------------|
| Basic         | O(n log n) | O(n log n) | O(n) | O(n log n) | O(1) | O(k) |
| Fast          | O(n²) | O(n) | O(1) | O(1) | O(1) | O(k) |
| Skip          | O(n log n) | O(log n) | O(log n) | O(1) | O(1) | O(k) |

#### `OrderbookLike` trait

```rust
pub trait OrderbookLike: Default {
    type BidIter<'a>: Iterator<Item = Level> where Self: 'a;
    type AskIter<'a>: Iterator<Item = Level> where Self: 'a;

    fn apply_snapshot(&mut self, bids: Vec<Level>, asks: Vec<Level>);
    fn apply_price_change(&mut self, side: Side, price: Price, size: Quantity);

    fn bids(&self) -> Self::BidIter<'_>;   // descending price
    fn asks(&self) -> Self::AskIter<'_>;   // ascending price

    // default impls derived from the iterators:
    fn best_bid(&self) -> Option<Level> { ... }
    fn best_ask(&self) -> Option<Level> { ... }
    fn spread(&self) -> Option<Price>   { ... }
}
```

The trait uses **Generic Associated Types** (`BidIter<'a>` / `AskIter<'a>`) to let each implementation return the cheapest iterator for its internal data structure — a slice iterator, a linked-list walker, etc. — without any shared buffer contract. `best_bid`, `best_ask`, and `spread` are default implementations that just consume the iterator; implementations do not need to override them. Cumulative sizes are not part of the trait — they are a derived scan that callers compute with a simple adapter on the iterator.

#### `BasicOrderbook`

The reference implementation backed by a sorted `Vec<Level>` per side. The iterator type is `std::iter::Copied<std::slice::Iter<'_, Level>>` — a zero-overhead slice walk. Used as the correctness reference in tests.

#### `FastOrderbook`

An intrusive doubly-linked list with a pool allocator and `HashMap<Decimal, u32>` for O(1) lookup by price tick.

Key design points:

- **`FastBookSide<const IS_BID: bool>`** — the sort direction is a const generic, not a runtime field. The ordering predicate `comes_before` resolves at compile time; the dead branch is eliminated, and the two sides (`FastBookSide<true>` for bids, `FastBookSide<false>` for asks) are separate monomorphised types.

- **Single node pool** — each `LevelNode` stores `{ price, size, next, prev }`. There is no separate level arena; price/size live directly in the node, removing one layer of indirection.

- **`FastBookSideIter`** — a lightweight iterator that follows `next` pointers through the node pool. No buffer is maintained; the list is traversed lazily on demand. Writes (insert/update/remove) touch only the intrusive list and the tick map — there is no cache to invalidate.

#### `SkipOrderbook`

A skip-list based implementation that provides O(log n) insertions and deletions while maintaining O(1) best-level access and O(k) iteration to depth k. The skip orderbook uses an intrusive skip-list structure with pool-allocated nodes and express-lane pointers that skip multiple levels, enabling efficient traversal to deep levels while preserving cache-friendly iteration patterns at the top of book.

Key design points:

- **`SkipBookSide<const IS_BID: bool>`** — similar to `FastOrderbook`, uses const generics for compile-time sort direction resolution.

- **Multi-level skip pointers** — each `LevelNode` maintains `forwards[0..MAX_HEIGHT]` where `forwards[0]` is the full linked-list pointer and higher levels provide express lanes that skip approximately 4^h nodes, achieving O(log k) traversal to depth k.

- **O(log n) insert/delete** — skip-list structure enables logarithmic-time operations for deep-level insertions and deletions, improving upon the O(n) complexity of the fast orderbook for operations away from the best level.