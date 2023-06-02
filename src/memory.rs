use core::cell::RefCell;

use alloc::vec::Vec;

pub type PhysAddr = usize;
pub type VirtAddr = usize;

pub struct Mempool {
    base_addr: *mut u8,
    num_entries: usize,
    entry_size: usize,
    phys_addresses: Vec<usize>,
    pub(crate) free_stack: RefCell<Vec<usize>>,
}
