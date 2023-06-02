//! Intel 82599+ 10Gb NIC Driver Implementation.

#![no_std]
#![deny(warnings)]
#![deny(missing_docs)]

mod constants;
mod hal;
mod ixgbe;
mod memory;
