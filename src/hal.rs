use crate::memory::PhysAddr;
use core::ptr::NonNull;
use core::time::Duration;

/// The interface which a particular hardware implementation must implement.
///
/// # Safety
///
/// Implementations of this trait must follow the "implementation safety" requirements documented
/// for each method. Callers must follow the safety requirements documented for the unsafe methods.
pub unsafe trait IxgbeHal
where
    Self: Sized,
{
    /// Allocates and zeroes the given number of contiguous physical memory of DMA memory for Ixgbe NIC
    /// use.
    ///
    /// Returns both the physical address which the device can use to access the memory, and a
    /// pointer to the start of it which the driver can use to access it.
    ///
    /// # Implementation safety
    ///
    /// Implementations of this method must ensure that the `NonNull<u8>` returned is a
    /// [_valid_](https://doc.rust-lang.org/std/ptr/index.html#safety) pointer, aligned to
    /// 2, and won't alias any other allocations or references in the program until it
    /// is deallocated by `dma_dealloc`. The pages must be zeroed.
    fn dma_alloc(size: usize) -> (PhysAddr, NonNull<u8>);

    /// Deallocates the given contiguous physical DMA memory pages.
    ///
    /// # Safety
    ///
    /// The memory must have been allocated by `dma_alloc` on the same `Hal` implementation, and not
    /// yet deallocated. `size` must be the same number passed to `dma_alloc` originally, and both
    /// `paddr` and `vaddr` must be the values returned by `dma_alloc`.
    unsafe fn dma_dealloc(paddr: PhysAddr, vaddr: NonNull<u8>, size: usize) -> i32;

    /// Converts a physical address used for MMIO to a virtual address which the driver can access.
    ///
    /// This is only used for MMIO addresses within BARs read from the device, for the PCI
    /// transport. It may check that the address range up to the given size is within the region
    /// expected for MMIO.
    ///
    /// # Implementation safety
    ///
    /// Implementations of this method must ensure that the `NonNull<u8>` returned is a
    /// [_valid_](https://doc.rust-lang.org/std/ptr/index.html#safety) pointer, and won't alias any
    /// other allocations or references in the program.
    ///
    /// # Safety
    ///
    /// The `paddr` and `size` must describe a valid MMIO region. The implementation may validate it
    /// in some way (and panic if it is invalid) but is not guaranteed to.
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8>;

    /// Converts a virtual address used by the driver to access MMIO to a physical address which the
    /// device can use.
    ///
    /// # Safety
    ///
    ///
    unsafe fn mmio_virt_to_phys(vaddr: NonNull<u8>, size: usize) -> PhysAddr;

    /// Returns the frequency of the TSC in Hz.
    fn get_tsc_frequency() -> u64;

    // /// Wait for the given number of microseconds.
    // fn wait_ms(microseconds: u32) -> Result<(), &'static str>;

    /// Wait until reaching the given deadline.
    fn wait_until(duration: Duration) -> Result<(), &'static str>;
}

#[allow(dead_code)]
/// The direction in which a buffer is passed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BufferDirection {
    /// The buffer may be read or written by the driver, but only read by the device.
    DriverToDevice,
    /// The buffer may be read or written by the device, but only read by the driver.
    DeviceToDriver,
    /// The buffer may be read or written by both the device and the driver.
    Both,
}
