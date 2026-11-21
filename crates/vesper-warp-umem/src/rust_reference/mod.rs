//! The Rust reference implementation, compiled beside the Vesper port so
//! the ABBA timing test and the differential tests can run both builds.

#[path = "../accounting.rs"]
pub mod accounting;
#[path = "../driver.rs"]
pub mod driver;
#[path = "../ring.rs"]
pub mod ring;
#[path = "../soft.rs"]
pub mod soft;
