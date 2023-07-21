use core::fmt::Debug;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::{cell::RefCell, marker::PhantomData};

use crate::hal::IxgbeHal;
use crate::{IxgbeError, IxgbeResult};
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

/// a Memory Pool struct to cache and accelerate memory allocation.
pub struct MemPool {
    base_addr: *mut u8,
    num_entries: usize,
    entry_size: usize,
    phys_addr: Vec<usize>,
    pub(crate) free_stack: RefCell<Vec<usize>>,
}

impl MemPool {
    /// Allocates a new `Mempool`.
    ///
    /// # Panics
    ///
    /// Panics if `size` is not a divisor of the page size.
    pub fn allocate<H: IxgbeHal>(entries: usize, size: usize) -> IxgbeResult<Arc<MemPool>> {
        let entry_size = match size {
            0 => 2048,
            x => x,
        };

        if HUGE_PAGE_SIZE % entry_size != 0 {
            error!("entry size must be a divisor of the page size");
            return Err(IxgbeError::PageNotAligned);
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

        let pool = MemPool {
            base_addr: dma.virt,
            num_entries: entries,
            entry_size,
            phys_addr,
            free_stack: RefCell::new(Vec::with_capacity(entries)),
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

        let mut free_stack = self.free_stack.borrow_mut();
        if free_stack.iter().any(|&x| x == id) {
            panic!("free buf: buffer already free");
        }

        free_stack.push(id);
    }

    /// Return entry size.
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
        // let size = if size % HUGE_PAGE_SIZE != 0 {
        //     ((size >> HUGE_PAGE_BITS) + 1) << HUGE_PAGE_BITS
        // } else {
        //     size
        // };
        // let size = if size < 0x1000 { 0x1000 } else { size };
        // let (pa, va) = H::dma_alloc(size / 0x1000, crate::BufferDirection::Both);
        let (pa, va) = H::dma_alloc(size);
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

pub struct Packet {
    pub(crate) addr_virt: NonNull<u8>,
    pub(crate) addr_phys: usize,
    pub(crate) len: usize,
    pub(crate) pool: Arc<MemPool>,
    pub(crate) pool_entry: usize,
}

impl Clone for Packet {
    fn clone(&self) -> Self {
        let mut p = alloc_pkt(&self.pool, self.len).expect("no buffer available");
        p.clone_from_slice(self);

        p
    }
}

impl Deref for Packet {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr_virt.as_ptr(), self.len) }
    }
}

impl DerefMut for Packet {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.addr_virt.as_ptr(), self.len) }
    }
}

impl Debug for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        self.pool.free_buf(self.pool_entry);
    }
}

impl Packet {
    /// Returns a new `Packet`.
    pub(crate) unsafe fn new(
        addr_virt: *mut u8,
        addr_phys: usize,
        len: usize,
        pool: Arc<MemPool>,
        pool_entry: usize,
    ) -> Packet {
        Packet {
            addr_virt: NonNull::new_unchecked(addr_virt),
            addr_phys,
            len,
            pool,
            pool_entry,
        }
    }
    /// Returns the virtual address of the packet.
    pub fn get_virt_addr(&self) -> *mut u8 {
        self.addr_virt.as_ptr()
    }

    /// Returns the physical address of the packet.
    pub fn get_phys_addr(&self) -> usize {
        self.addr_phys
    }

    /// Returns all data in the buffer, not including header.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.addr_virt.as_ptr(), self.len) }
    }

    /// Returns all data in the buffer with the mutuable reference,
    /// not including header.
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.addr_virt.as_ptr(), self.len) }
    }

    /// Returns a mutable slice to the headroom of the pakcet.
    ///
    /// The `len` parameter controls how much of the headroom is returned.
    ///
    /// # Panics
    ///
    /// Panics if `len` is greater than [`PACKET_HEADROOM`]
    pub fn headroom_mut(&mut self, len: usize) -> &mut [u8] {
        assert!(len <= PACKET_HEADROOM);
        unsafe { slice::from_raw_parts_mut(self.addr_virt.as_ptr().sub(len), len) }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub(crate) fn prefrtch(&self, hint: Prefetch) {
        if core_detect::is_x86_feature_detected!("sse") {
            let addr = self.get_virt_addr() as *const _;
            unsafe {
                use core::arch::x86_64;
                match hint {
                    Prefetch::Time0 => x86_64::_mm_prefetch(addr, x86_64::_MM_HINT_T0),
                    Prefetch::Time1 => x86_64::_mm_prefetch(addr, x86_64::_MM_HINT_T1),
                    Prefetch::Time2 => x86_64::_mm_prefetch(addr, x86_64::_MM_HINT_T2),
                    Prefetch::NonTemporal => x86_64::_mm_prefetch(addr, x86_64::_MM_HINT_NTA),
                }
            }
        }
    }
}

/// Returns a free packet from the `pool`, or [`None`] if the requested packet size exceeds the
/// maximum size for that pool or if the pool is empty.
pub fn alloc_pkt(pool: &Arc<MemPool>, size: usize) -> Option<Packet> {
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

unsafe impl Sync for MemPool {}
unsafe impl Send for MemPool {}
