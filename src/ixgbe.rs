use crate::memory::{memset, Dma, Mempool};
use crate::NicDevice;
use crate::{constants::*, hal::IxgbeHal};
use alloc::{collections::VecDeque, rc::Rc, vec::Vec};
use core::marker::PhantomData;
use core::{mem, ptr};

pub enum IxgbeError {
    QueueNotAligned,
}
pub type IxgbeResult<T = ()> = Result<T, IxgbeError>;

const DRIVER_NAME: &str = "ixy-ixgbe";

const MAX_QUEUES: u16 = 64;

const PKT_BUF_ENTRY_SIZE: usize = 2048;
const MIN_MEMPOOL_SIZE: usize = 4096;

const NUM_RX_QUEUE_ENTRIES: usize = 512;
const NUM_TX_QUEUE_ENTRIES: usize = 512;
const TX_CLEAN_BATCH: usize = 32;

pub struct IxgbeDevice<H: IxgbeHal> {
    // pci_addr: String,
    addr: *mut u8,
    len: usize,
    num_rx_queues: u16,
    num_tx_queues: u16,
    rx_queues: Vec<IxgbeRxQueue<H>>,
    tx_queues: Vec<IxgbeTxQueue<H>>,
    // interrupts: Interrupts,
    _marker: PhantomData<H>,
}

struct IxgbeRxQueue<H: IxgbeHal> {
    descriptors: *mut ixgbe_adv_rx_desc,
    num_descriptors: usize,
    pool: Rc<Mempool<H>>,
    bufs_in_use: Vec<usize>,
    rx_index: usize,
}

struct IxgbeTxQueue<H: IxgbeHal> {
    descriptors: *mut ixgbe_adv_tx_desc,
    num_descriptors: usize,
    pool: Option<Rc<Mempool<H>>>,
    bufs_in_use: VecDeque<usize>,
    clean_index: usize,
    tx_index: usize,
}

impl<H: IxgbeHal> NicDevice<H> for IxgbeDevice<H> {
    fn get_driver_name(&self) -> &str {
        todo!()
    }

    fn get_link_speed(&self) -> u16 {
        todo!()
    }

    fn get_mac_addr(&self) -> [u8; 6] {
        todo!()
    }

    fn get_pci_addr(&self) -> &str {
        todo!()
    }

    fn read_stats(&self, stats: &mut crate::DeviceStats) {
        todo!()
    }

    fn reset_stats(&mut self) {
        todo!()
    }

    fn rx_batch(
        &mut self,
        queue_id: u16,
        buffer: &mut VecDeque<crate::memory::Packet<H>>,
        num_packets: usize,
    ) -> usize {
        todo!()
    }

    fn set_mac_addr(&self, mac: [u8; 6]) {
        todo!()
    }

    fn tx_batch(
        &mut self,
        queue_id: u16,
        buffer: &mut VecDeque<crate::memory::Packet<H>>,
    ) -> usize {
        todo!()
    }

    fn tx_batch_busy_wait(
        &mut self,
        queue_id: u16,
        buffer: &mut VecDeque<crate::memory::Packet<H>>,
    ) {
        todo!()
    }
}

impl<H: IxgbeHal> IxgbeDevice<H> {
    pub fn init(
        base: usize,
        len: usize,
        num_rx_queues: u16,
        num_tx_queues: u16,
    ) -> IxgbeResult<Self> {
        info!(
            "Initializing ixgbe device@base: {:#x}, len: {:#x}, num_rx_queues: {}, num_tx_queues: {}",
            base, len, num_rx_queues, num_tx_queues
        );
        // initialize RX and TX queue
        let rx_queues = Vec::with_capacity(num_rx_queues as usize);
        let tx_queues = Vec::with_capacity(num_tx_queues as usize);

        let mut dev = IxgbeDevice {
            addr: base as *mut u8,
            len,
            num_rx_queues,
            num_tx_queues,
            rx_queues,
            tx_queues,
            _marker: PhantomData,
        };
        dev.reset_and_init()?;
        Ok(dev)
    }
}

// Private methods implementation
impl<H: IxgbeHal> IxgbeDevice<H> {
    /// Resets and initializes the device.
    fn reset_and_init(&mut self) -> IxgbeResult {
        info!("resetting device ixgbe device");
        // section 4.6.3.1 - disable all interrupts
        self.disable_interrupts();

        // section 4.6.3.2
        self.set_reg32(IXGBE_CTRL, IXGBE_CTRL_RST_MASK);
        self.wait_clear_reg32(IXGBE_CTRL, IXGBE_CTRL_RST_MASK);
        // TODO: sleep 10 millis.
        // thread::sleep(Duration::from_millis(10));

        // section 4.6.3.1 - disable interrupts again after reset
        self.disable_interrupts();

        let mac = self.get_mac_addr();
        info!(
            "mac address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        // section 4.6.3 - wait for EEPROM auto read completion
        self.wait_set_reg32(IXGBE_EEC, IXGBE_EEC_ARD);

        // section 4.6.3 - wait for dma initialization done
        self.wait_set_reg32(IXGBE_RDRXCTL, IXGBE_RDRXCTL_DMAIDONE);

        // skip last step from 4.6.3 - we don't want interrupts

        // section 4.6.4 - initialize link (auto negotiation)
        self.init_link();

        // section 4.6.5 - statistical counters
        // reset-on-read registers, just read them once
        self.reset_stats();

        // section 4.6.7 - init rx
        self.init_rx()?;

        // section 4.6.8 - init tx
        self.init_tx()?;

        for i in 0..self.num_rx_queues {
            self.start_rx_queue(i)?;
        }

        for i in 0..self.num_tx_queues {
            self.start_tx_queue(i)?;
        }

        // enable interrupts
        for queue in 0..self.num_rx_queues {
            self.enable_interrupt(queue)?;
        }

        // enable promisc mode by default to make testing easier
        self.set_promisc(true);

        // wait some time for the link to come up
        self.wait_for_link();

        Ok(())
    }

    // sections 4.6.7
    /// Initializes the rx queues of this device.
    fn init_rx(&mut self) -> IxgbeResult {
        // disable rx while re-configuring it
        self.clear_flags32(IXGBE_RXCTRL, IXGBE_RXCTRL_RXEN);

        // section 4.6.11.3.4 - allocate all queues and traffic to PB0
        self.set_reg32(IXGBE_RXPBSIZE(0), IXGBE_RXPBSIZE_128KB);
        for i in 1..8 {
            self.set_reg32(IXGBE_RXPBSIZE(i), 0);
        }

        // enable CRC offloading
        self.set_flags32(IXGBE_HLREG0, IXGBE_HLREG0_RXCRCSTRP);
        self.set_flags32(IXGBE_RDRXCTL, IXGBE_RDRXCTL_CRCSTRIP);

        // accept broadcast packets
        self.set_flags32(IXGBE_FCTRL, IXGBE_FCTRL_BAM);

        // configure queues, same for all queues
        for i in 0..self.num_rx_queues {
            debug!("initializing rx queue {}", i);
            // enable advanced rx descriptors
            self.set_reg32(
                IXGBE_SRRCTL(u32::from(i)),
                (self.get_reg32(IXGBE_SRRCTL(u32::from(i))) & !IXGBE_SRRCTL_DESCTYPE_MASK)
                    | IXGBE_SRRCTL_DESCTYPE_ADV_ONEBUF,
            );
            // let nic drop packets if no rx descriptor is available instead of buffering them
            self.set_flags32(IXGBE_SRRCTL(u32::from(i)), IXGBE_SRRCTL_DROP_EN);

            // section 7.1.9 - setup descriptor ring
            let ring_size_bytes =
                (NUM_RX_QUEUE_ENTRIES) as usize * mem::size_of::<ixgbe_adv_rx_desc>();

            let dma: Dma<ixgbe_adv_rx_desc, H> = Dma::allocate(ring_size_bytes, true)?;

            // initialize to 0xff to prevent rogue memory accesses on premature dma activation
            unsafe {
                memset(dma.virt as *mut u8, ring_size_bytes, 0xff);
            }

            self.set_reg32(
                IXGBE_RDBAL(u32::from(i)),
                (dma.phys as u64 & 0xffff_ffff) as u32,
            );
            self.set_reg32(IXGBE_RDBAH(u32::from(i)), (dma.phys as u64 >> 32) as u32);
            self.set_reg32(IXGBE_RDLEN(u32::from(i)), ring_size_bytes as u32);

            debug!("rx ring {} phys addr: {:#x}", i, dma.phys);
            debug!("rx ring {} virt addr: {:p}", i, dma.virt);

            // set ring to empty at start
            self.set_reg32(IXGBE_RDH(u32::from(i)), 0);
            self.set_reg32(IXGBE_RDT(u32::from(i)), 0);

            let mempool_size = if NUM_RX_QUEUE_ENTRIES + NUM_TX_QUEUE_ENTRIES < MIN_MEMPOOL_SIZE {
                MIN_MEMPOOL_SIZE
            } else {
                NUM_RX_QUEUE_ENTRIES + NUM_TX_QUEUE_ENTRIES
            };

            let mempool = Mempool::allocate(mempool_size as usize, PKT_BUF_ENTRY_SIZE).unwrap();

            let rx_queue = IxgbeRxQueue {
                descriptors: dma.virt,
                pool: mempool,
                num_descriptors: NUM_RX_QUEUE_ENTRIES,
                rx_index: 0,
                bufs_in_use: Vec::with_capacity(NUM_RX_QUEUE_ENTRIES),
            };

            self.rx_queues.push(rx_queue);
        }

        // last sentence of section 4.6.7 - set some magic bits
        self.set_flags32(IXGBE_CTRL_EXT, IXGBE_CTRL_EXT_NS_DIS);

        // probably a broken feature, this flag is initialized with 1 but has to be set to 0
        for i in 0..self.num_rx_queues {
            self.clear_flags32(IXGBE_DCA_RXCTRL(u32::from(i)), 1 << 12);
        }

        // start rx
        self.set_flags32(IXGBE_RXCTRL, IXGBE_RXCTRL_RXEN);

        Ok(())
    }

    // section 4.6.8
    /// Initializes the tx queues of this device.
    fn init_tx(&mut self) -> IxgbeResult {
        // crc offload and small packet padding
        self.set_flags32(IXGBE_HLREG0, IXGBE_HLREG0_TXCRCEN | IXGBE_HLREG0_TXPADEN);

        // section 4.6.11.3.4 - set default buffer size allocations
        self.set_reg32(IXGBE_TXPBSIZE(0), IXGBE_TXPBSIZE_40KB);
        for i in 1..8 {
            self.set_reg32(IXGBE_TXPBSIZE(i), 0);
        }

        // required when not using DCB/VTd
        self.set_reg32(IXGBE_DTXMXSZRQ, 0xffff);
        self.clear_flags32(IXGBE_RTTDCS, IXGBE_RTTDCS_ARBDIS);

        // configure queues
        for i in 0..self.num_tx_queues {
            debug!("initializing tx queue {}", i);
            // section 7.1.9 - setup descriptor ring
            let ring_size_bytes =
                NUM_TX_QUEUE_ENTRIES as usize * mem::size_of::<ixgbe_adv_tx_desc>();

            let dma: Dma<ixgbe_adv_tx_desc, H> = Dma::allocate(ring_size_bytes, true)?;
            unsafe {
                memset(dma.virt as *mut u8, ring_size_bytes, 0xff);
            }

            self.set_reg32(
                IXGBE_TDBAL(u32::from(i)),
                (dma.phys as u64 & 0xffff_ffff) as u32,
            );
            self.set_reg32(IXGBE_TDBAH(u32::from(i)), (dma.phys as u64 >> 32) as u32);
            self.set_reg32(IXGBE_TDLEN(u32::from(i)), ring_size_bytes as u32);

            debug!("tx ring {} phys addr: {:#x}", i, dma.phys);
            debug!("tx ring {} virt addr: {:p}", i, dma.virt);

            // descriptor writeback magic values, important to get good performance and low PCIe overhead
            // see 7.2.3.4.1 and 7.2.3.5 for an explanation of these values and how to find good ones
            // we just use the defaults from DPDK here, but this is a potentially interesting point for optimizations
            let mut txdctl = self.get_reg32(IXGBE_TXDCTL(u32::from(i)));
            // there are no defines for this in constants.rs for some reason
            // pthresh: 6:0, hthresh: 14:8, wthresh: 22:16
            txdctl &= !(0x7F | (0x7F << 8) | (0x7F << 16));
            txdctl |= 36 | (8 << 8) | (4 << 16);

            self.set_reg32(IXGBE_TXDCTL(u32::from(i)), txdctl);

            let tx_queue = IxgbeTxQueue {
                descriptors: dma.virt,
                bufs_in_use: VecDeque::with_capacity(NUM_TX_QUEUE_ENTRIES),
                pool: None,
                num_descriptors: NUM_TX_QUEUE_ENTRIES,
                clean_index: 0,
                tx_index: 0,
            };

            self.tx_queues.push(tx_queue);
        }

        // final step: enable DMA
        self.set_reg32(IXGBE_DMATXCTL, IXGBE_DMATXCTL_TE);

        Ok(())
    }

    /// Sets the rx queues` descriptors and enables the queues.
    fn start_rx_queue(&mut self, queue_id: u16) -> IxgbeResult {
        debug!("starting rx queue {}", queue_id);

        {
            let queue = &mut self.rx_queues[queue_id as usize];

            if queue.num_descriptors & (queue.num_descriptors - 1) != 0 {
                // return Err("number of queue entries must be a power of 2".into());
                return Err(IxgbeError::QueueNotAligned);
            }

            for i in 0..queue.num_descriptors {
                let pool = &queue.pool;

                let buf = match pool.alloc_buf() {
                    Some(x) => x,
                    None => return Err("failed to allocate rx descriptor".into()),
                };

                unsafe {
                    ptr::write_volatile(
                        &mut (*queue.descriptors.add(i)).read.pkt_addr as *mut u64,
                        pool.get_phys_addr(buf) as u64,
                    );

                    ptr::write_volatile(
                        &mut (*queue.descriptors.add(i)).read.hdr_addr as *mut u64,
                        0,
                    );
                }

                // we need to remember which descriptor entry belongs to which mempool entry
                queue.bufs_in_use.push(buf);
            }
        }

        let queue = &self.rx_queues[queue_id as usize];

        // enable queue and wait if necessary
        self.set_flags32(IXGBE_RXDCTL(u32::from(queue_id)), IXGBE_RXDCTL_ENABLE);
        self.wait_set_reg32(IXGBE_RXDCTL(u32::from(queue_id)), IXGBE_RXDCTL_ENABLE);

        // rx queue starts out full
        self.set_reg32(IXGBE_RDH(u32::from(queue_id)), 0);

        // was set to 0 before in the init function
        self.set_reg32(
            IXGBE_RDT(u32::from(queue_id)),
            (queue.num_descriptors - 1) as u32,
        );

        Ok(())
    }

    /// Enables the tx queues.
    fn start_tx_queue(&mut self, queue_id: u16) -> IxgbeResult {
        debug!("starting tx queue {}", queue_id);

        {
            let queue = &mut self.tx_queues[queue_id as usize];

            if queue.num_descriptors & (queue.num_descriptors - 1) != 0 {
                // return Err("number of queue entries must be a power of 2".into());
                return Err(IxgbeError::QueueNotAligned);
            }
        }

        // tx queue starts out empty
        self.set_reg32(IXGBE_TDH(u32::from(queue_id)), 0);
        self.set_reg32(IXGBE_TDT(u32::from(queue_id)), 0);

        // enable queue and wait if necessary
        self.set_flags32(IXGBE_TXDCTL(u32::from(queue_id)), IXGBE_TXDCTL_ENABLE);
        self.wait_set_reg32(IXGBE_TXDCTL(u32::from(queue_id)), IXGBE_TXDCTL_ENABLE);

        Ok(())
    }

    // see section 4.6.4
    /// Initializes the link of this device.
    fn init_link(&self) {
        // link auto-configuration register should already be set correctly, we're resetting it anyway
        self.set_reg32(
            IXGBE_AUTOC,
            (self.get_reg32(IXGBE_AUTOC) & !IXGBE_AUTOC_LMS_MASK) | IXGBE_AUTOC_LMS_10G_SERIAL,
        );
        self.set_reg32(
            IXGBE_AUTOC,
            (self.get_reg32(IXGBE_AUTOC) & !IXGBE_AUTOC_10G_PMA_PMD_MASK) | IXGBE_AUTOC_10G_XAUI,
        );
        // negotiate link
        self.set_flags32(IXGBE_AUTOC, IXGBE_AUTOC_AN_RESTART);
        // datasheet wants us to wait for the link here, but we can continue and wait afterwards
    }

    /// Disable all interrupts for all queues.
    fn disable_interrupts(&self) {
        // Clear interrupt mask to stop from interrupts being generated
        self.set_reg32(IXGBE_EIMS, 0x0000_0000);
        self.clear_interrupts();
    }

    /// Clear all interrupt masks for all queues.
    fn clear_interrupts(&self) {
        // Clear interrupt mask
        self.set_reg32(IXGBE_EIMC, IXGBE_IRQ_CLEAR_MASK);
        self.get_reg32(IXGBE_EICR);
    }

    /// Returns the register at `self.addr` + `reg`.
    ///
    /// # Panics
    ///
    /// Panics if `self.addr` + `reg` does not belong to the mapped memory of the pci device.
    fn get_reg32(&self, reg: u32) -> u32 {
        assert!(reg as usize <= self.len - 4, "memory access out of bounds");

        unsafe { ptr::read_volatile((self.addr as usize + reg as usize) as *mut u32) }
    }

    /// Sets the register at `self.addr` + `reg` to `value`.
    ///
    /// # Panics
    ///
    /// Panics if `self.addr` + `reg` does not belong to the mapped memory of the pci device.
    fn set_reg32(&self, reg: u32, value: u32) {
        assert!(reg as usize <= self.len - 4, "memory access out of bounds");

        unsafe {
            ptr::write_volatile((self.addr as usize + reg as usize) as *mut u32, value);
        }
    }

    /// Sets the `flags` at `self.addr` + `reg`.
    fn set_flags32(&self, reg: u32, flags: u32) {
        self.set_reg32(reg, self.get_reg32(reg) | flags);
    }

    /// Clears the `flags` at `self.addr` + `reg`.
    fn clear_flags32(&self, reg: u32, flags: u32) {
        self.set_reg32(reg, self.get_reg32(reg) & !flags);
    }

    /// Waits for `self.addr` + `reg` to clear `value`.
    fn wait_clear_reg32(&self, reg: u32, value: u32) {
        loop {
            let current = self.get_reg32(reg);
            if (current & value) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }

    /// Waits for `self.addr` + `reg` to set `value`.
    fn wait_set_reg32(&self, reg: u32, value: u32) {
        loop {
            let current = self.get_reg32(reg);
            if (current & value) == value {
                break;
            }
            core::hint::spin_loop();
        }
    }
}
