//! Optional I/O adapters that wrap the sans-I/O core into convenience APIs.
//!
//! Each adapter is gated behind its own feature flag so users only pay for
//! what they use. Implementation lands in Stage 6 of the development plan.

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub mod tokio;

#[cfg(feature = "serial")]
#[cfg_attr(docsrs, doc(cfg(feature = "serial")))]
pub mod serialport;
