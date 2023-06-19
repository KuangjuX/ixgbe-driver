//! Intel 82599+ 10Gb NIC Driver Implementation.

#![no_std]
#![deny(warnings)]
#![deny(missing_docs)]
#![allow(dead_code)]

mod constants;
mod hal;
mod interrupts;
mod ixgbe;
mod memory;

extern crate alloc;
#[macro_use]
extern crate log;

use crate::memory::Packet;
use alloc::collections::VecDeque;

pub use hal::{BufferDirection, IxgbeHal};
pub use ixgbe::{IxgbeDevice, IxgbeError, IxgbeResult, RxBuffer};
pub use memory::PhysAddr;

/// Vendor ID for Intel.
pub const INTEL_VEND: u16 = 0x8086;

/// Device ID for the 82599ES, used to identify the device from the PCI space.
pub const INTEL_82599: u16 = 0x10FB;

/// Used for implementing an ixy device driver like ixgbe or virtio.
pub trait NicDevice<H: IxgbeHal> {
    /// Returns the driver's name.
    fn get_driver_name(&self) -> &str;

    /// Returns the pci address of this device.
    fn get_pci_addr(&self) -> &str;

    /// Returns the layer 2 address of this device.
    fn get_mac_addr(&self) -> [u8; 6];

    /// Sets the layer 2 address of this device.
    fn set_mac_addr(&self, mac: [u8; 6]);

    /// Pushes up to `num_packets` `Packet`s onto `buffer` depending on the amount of
    /// received packets by the network card. Returns the number of received packets.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use ixy::*;
    /// use ixy::memory::Packet;
    /// use std::collections::VecDeque;
    ///
    /// let mut dev = ixy_init("0000:01:00.0", 1, 1, 0).unwrap();
    /// let mut buf: VecDeque<Packet> = VecDeque::new();
    ///
    /// dev.rx_batch(0, &mut buf, 32);
    /// ```
    fn rx_batch(
        &mut self,
        queue_id: u16,
        buffer: &mut VecDeque<Packet<H>>,
        num_packets: usize,
    ) -> usize;

    /// Takes `Packet`s out of `buffer` until `buffer` is empty or the network card's tx
    /// queue is full. Returns the number of sent packets.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use ixy::*;
    /// use ixy::memory::Packet;
    /// use std::collections::VecDeque;
    ///
    /// let mut dev = ixy_init("0000:01:00.0", 1, 1, 0).unwrap();
    /// let mut buf: VecDeque<Packet> = VecDeque::new();
    ///
    /// assert_eq!(dev.tx_batch(0, &mut buf), 0);
    /// ```
    fn tx_batch(&mut self, queue_id: u16, buffer: &mut VecDeque<Packet<H>>) -> usize;

    /// Reads the network card's stats registers into `stats`.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use ixy::*;
    ///
    /// let mut dev = ixy_init("0000:01:00.0", 1, 1, 0).unwrap();
    /// let mut stats: DeviceStats = Default::default();
    ///
    /// dev.read_stats(&mut stats);
    /// ```
    fn read_stats(&self, stats: &mut DeviceStats);

    /// Resets the network card's stats registers.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use ixy::*;
    ///
    /// let mut dev = ixy_init("0000:01:00.0", 1, 1, 0).unwrap();
    /// dev.reset_stats();
    /// ```
    fn reset_stats(&mut self);

    /// Returns the network card's link speed.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use ixy::*;
    ///
    /// let mut dev = ixy_init("0000:01:00.0", 1, 1, 0).unwrap();
    /// println!("Link speed is {} Mbit/s", dev.get_link_speed());
    /// ```
    fn get_link_speed(&self) -> u16;

    /// Takes `Packet`s out of `buffer` to send out. This will busy wait until all packets from
    /// `buffer` are queued.
    fn tx_batch_busy_wait(&mut self, queue_id: u16, buffer: &mut VecDeque<Packet<H>>) {
        while !buffer.is_empty() {
            self.tx_batch(queue_id, buffer);
        }
    }

    /// Receives a [`RxBuffer`] from network. If currently no data, returns an error
    /// with type [`IxgbeError::NotReady`].
    ///
    /// It will try to pop a buffer that completed data reception in the NIC queue.
    fn receive(&mut self, queue_id: u16, size: usize) -> IxgbeResult<RxBuffer<H>>;
}

/// Holds network card stats about sent and received packets.
#[allow(missing_docs)]
#[derive(Default, Copy, Clone)]
pub struct DeviceStats {
    pub rx_pkts: u64,
    pub tx_pkts: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl core::fmt::Display for DeviceStats {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "rx_pkts: {}, tx_pkts: {}, rx_bytes: {}, tx_bytes: {}",
            self.rx_pkts, self.tx_pkts, self.rx_bytes, self.tx_bytes
        )
    }
}

// /// Initializes the network card with the given `pci_addr` and returns a `NicDevice`.
// pub fn init_ixgbe<H: IxgbeHal>() -> IxgbeResult<IxgbeDevice<H>> {}
