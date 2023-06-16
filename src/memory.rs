use core::fmt::Debug;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::{cell::RefCell, marker::PhantomData};

use crate::{hal::IxgbeHal, ixgbe::IxgbeResult};
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::{fmt, slice};

/// Phyaical Address
pub type PhysAddr = usize;
/// Virtual Address
pub type VirtAddr = usize;

const HUGE_PAGE_BITS: u32 = 21;
const HUGE_PAGE_SIZE: usize = 1 << HUGE_PAGE_BITS;

// this differs from upstream ixy as our packet metadata is stored outside of the actual packet data
// which results in a different alignment requirement
pub const PACKET_HEADROOM: usize = 32;

pub struct Mempool<H: IxgbeHal> {
    base_addr: *mut u8,
    num_entries: usize,
    entry_size: usize,
    phys_addr: Vec<usize>,
    pub(crate) free_stack: RefCell<Vec<usize>>,
    _marker: PhantomData<H>,
}

impl<H: IxgbeHal> Mempool<H> {
    /// Allocates a new `Mempool`.
    ///
    /// # Panics
    ///
    /// Panics if `size` is not a divisor of the page size.
    pub fn allocate(entries: usize, size: usize) -> IxgbeResult<Arc<Mempool<H>>> {
        let entry_size = match size {
            0 => 2048,
            x => x,
        };

        if HUGE_PAGE_SIZE % entry_size != 0 {
            error!("entry size must be a divisor of the page size");
            return Err(crate::ixgbe::IxgbeError::PageNotAligned);
        }

        let dma = Dma::<u8, H>::allocate(entries * entry_size, false)?;
        let mut phys_addr = Vec::with_capacity(entries);

        for i in 0..entries {
            phys_addr.push(unsafe {
                H::mmio_virt_to_phys(
                    NonNull::new(dma.virt.add(i * entry_size)).unwrap(),
                    entry_size,
                )
            })
        }

        let pool = Mempool::<H> {
            base_addr: dma.virt,
            num_entries: entries,
            entry_size,
            phys_addr,
            free_stack: RefCell::new(Vec::with_capacity(entries)),
            _marker: PhantomData,
        };

        let pool = Arc::new(pool);
        pool.free_stack.borrow_mut().extend(0..entries);

        Ok(pool)
    }

    /// Returns the position of a free buffer in the memory pool, or [`None`] if the pool is empty.
    pub(crate) fn alloc_buf(&self) -> Option<usize> {
        self.free_stack.borrow_mut().pop()
    }

    /// Marks a buffer in the memory pool as free.
    pub(crate) fn free_buf(&self, id: usize) {
        assert!(id < self.num_entries, "buffer outside of memory pool");

        self.free_stack.borrow_mut().push(id);
    }

    pub fn entry_size(&self) -> usize {
        self.entry_size
    }

    /// Returns the virtual address of a buffer from the memory pool.
    pub(crate) fn get_virt_addr(&self, id: usize) -> *mut u8 {
        assert!(id < self.num_entries, "buffer outside of memory pool");

        unsafe { self.base_addr.add(id * self.entry_size) }
    }

    /// Returns the physical address of a buffer from the memory pool.
    pub fn get_phys_addr(&self, id: usize) -> usize {
        self.phys_addr[id]
    }
}

pub struct Dma<T, H: IxgbeHal> {
    pub virt: *mut T,
    pub phys: usize,
    _marker: PhantomData<H>,
}

impl<T, H: IxgbeHal> Dma<T, H> {
    pub fn allocate(size: usize, _require_contiguous: bool) -> IxgbeResult<Dma<T, H>> {
        let size = if size % HUGE_PAGE_SIZE != 0 {
            ((size >> HUGE_PAGE_BITS) + 1) << HUGE_PAGE_BITS
        } else {
            size
        };
        let (pa, va) = H::dma_alloc(size / 0x1000, crate::BufferDirection::Both);
        info!(
            "allocated DMA memory @pa: {:#x}, va: {:#x}, size: {:#x}",
            pa,
            va.as_ptr() as usize,
            size
        );
        Ok(Dma::<T, H> {
            virt: va.as_ptr() as *mut T,
            phys: pa,
            _marker: PhantomData,
        })
    }
}

pub struct Packet<H: IxgbeHal> {
    pub(crate) addr_virt: *mut u8,
    pub(crate) addr_phys: usize,
    pub(crate) len: usize,
    pub(crate) pool: Arc<Mempool<H>>,
    pub(crate) pool_entry: usize,
    pub(crate) _marker: PhantomData<H>,
}

impl<H: IxgbeHal> Clone for Packet<H> {
    fn clone(&self) -> Self {
        let mut p = alloc_pkt(&self.pool, self.len).expect("no buffer available");
        p.clone_from_slice(self);

        p
    }
}

impl<H: IxgbeHal> Deref for Packet<H> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr_virt, self.len) }
    }
}

impl<H: IxgbeHal> DerefMut for Packet<H> {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.addr_virt, self.len) }
    }
}

impl<H: IxgbeHal> Debug for Packet<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<H: IxgbeHal> Drop for Packet<H> {
    fn drop(&mut self) {
        self.pool.free_buf(self.pool_entry);
    }
}

impl<H: IxgbeHal> Packet<H> {
    /// Returns a new `Packet`.
    pub(crate) unsafe fn new(
        addr_virt: *mut u8,
        addr_phys: usize,
        len: usize,
        pool: Arc<Mempool<H>>,
        pool_entry: usize,
    ) -> Packet<H> {
        Packet::<H> {
            addr_virt,
            addr_phys,
            len,
            pool,
            pool_entry,
            _marker: PhantomData,
        }
    }
    /// Returns the virtual address of the packet.
    pub fn get_virt_addr(&self) -> *mut u8 {
        self.addr_virt
    }

    /// Returns the physical address of the packet.
    pub fn get_phys_addr(&self) -> usize {
        self.addr_phys
    }
}

/// Returns a free packet from the `pool`, or [`None`] if the requested packet size exceeds the
/// maximum size for that pool or if the pool is empty.
pub fn alloc_pkt<H: IxgbeHal>(pool: &Arc<Mempool<H>>, size: usize) -> Option<Packet<H>> {
    if size > pool.entry_size - PACKET_HEADROOM {
        return None;
    }

    pool.alloc_buf().map(|id| unsafe {
        Packet::new(
            pool.get_virt_addr(id).add(PACKET_HEADROOM),
            pool.get_phys_addr(id) + PACKET_HEADROOM,
            size,
            Arc::clone(pool),
            id,
        )
    })
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
