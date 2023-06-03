use core::{cell::RefCell, marker::PhantomData};

use crate::{hal::IxgbeHal, ixgbe::IxgbeResult};
use alloc::rc::Rc;
use alloc::vec::Vec;

pub type PhysAddr = usize;
pub type VirtAddr = usize;

pub struct Mempool<H: IxgbeHal> {
    base_addr: *mut u8,
    num_entries: usize,
    entry_size: usize,
    phys_addresses: Vec<usize>,
    pub(crate) free_stack: RefCell<Vec<usize>>,
    _marker: PhantomData<H>,
}

impl<H: IxgbeHal> Mempool<H> {
    /// Allocates a new `Mempool`.
    ///
    /// # Panics
    ///
    /// Panics if `size` is not a divisor of the page size.
    pub fn allocate(entries: usize, size: usize) -> IxgbeResult<Rc<Mempool<H>>> {
        todo!()
    }

    pub(crate) fn alloc_buf(&self) -> Option<VirtAddr> {
        todo!()
    }
}

pub struct Dma<T, H: IxgbeHal> {
    pub virt: *mut T,
    pub phys: usize,
    _marker: PhantomData<H>,
}

impl<T, H: IxgbeHal> Dma<T, H> {
    pub fn allocate(size: usize, require_contiguous: bool) -> IxgbeResult<Dma<T, H>> {
        todo!()
    }
}

pub struct Packet<H: IxgbeHal> {
    pub(crate) addr_virt: *mut u8,
    pub(crate) addr_phys: usize,
    pub(crate) len: usize,
    pub(crate) pool: Rc<Mempool<H>>,
    pub(crate) pool_entry: usize,
    _marker: PhantomData<H>,
}

/// Initializes `len` fields of type `T` at `addr` with `value`.
pub(crate) unsafe fn memset<T: Copy>(addr: *mut T, len: usize, value: T) {
    for i in 0..len {
        core::ptr::write_volatile(addr.add(i) as *mut T, value);
    }
}

/// Common representation for prefetch strategies.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prefetch {
    /// Corresponds to _MM_HINT_T0 on x86 sse.
    Time0,

    /// Corresponds to _MM_HINT_T1 on x86 sse.
    Time1,

    /// Corresponds to _MM_HINT_T2 on x86 sse.
    Time2,

    /// Corresponds to _MM_HINT_NTA on x86 sse.
    NonTemporal,
}
