//! Transport abstractions.
//!
//! `Transport` is the synchronous interface adapter modules build on; it
//! mirrors `std::io::{Read, Write}` minus the parts the protocol does not
//! need. `AsyncTransport` is the async equivalent for the `tokio` adapter.
//!
//! Implementation lands in Stage 3+ of the development plan; concrete
//! implementations are provided by the optional `serial` adapter.
