//! Size ceilings for bodies Yabane holds in memory.
//!
//! One ceiling covers the three places where an exchange can be buffered whole:
//! the caller request body Yabane reads before resolving a route, a
//! non-streaming Provider response buffered for protocol conversion, and output
//! accumulated for a caller that needs the complete answer. A single definition
//! keeps that memory posture readable here instead of drifting across copies.

/// Greatest number of payload bytes Yabane buffers for one exchange.
pub const MAX_BUFFERED_BODY_BYTES: usize = 128 * 1024 * 1024;
