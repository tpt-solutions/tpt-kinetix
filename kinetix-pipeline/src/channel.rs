//! Typed inter-stage channel wrappers with backpressure semantics.
//!
//! TODO (Phase 5): Implement bounded channels with configurable capacity and
//! backpressure metrics.

/// Default inter-stage channel capacity (number of items that can be buffered
/// between two adjacent stages before the producer blocks).
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;
