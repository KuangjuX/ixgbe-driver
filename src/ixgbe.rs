use crate::constants::{ixgbe_adv_rx_desc, ixgbe_adv_tx_desc};
use crate::memory::Mempool;
use alloc::{collections::VecDeque, rc::Rc, string::String, vec::Vec};

pub struct IxgbeDevice {
    pci_addr: String,
    addr: *mut u8,
    len: usize,
    num_rx_queues: u16,
    num_tx_queues: u16,
    rx_queues: Vec<IxgbeRxQueue>,
    tx_queues: Vec<IxgbeTxQueue>,
    // interrupts: Interrupts,
}

struct IxgbeRxQueue {
    descriptors: *mut ixgbe_adv_rx_desc,
    num_descriptors: usize,
    pool: Rc<Mempool>,
    bufs_in_use: Vec<usize>,
    rx_index: usize,
}

struct IxgbeTxQueue {
    descriptors: *mut ixgbe_adv_tx_desc,
    num_descriptors: usize,
    pool: Option<Rc<Mempool>>,
    bufs_in_use: VecDeque<usize>,
    clean_index: usize,
    tx_index: usize,
}
