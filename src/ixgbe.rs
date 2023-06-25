use crate::interrupts::Interrupts;
use crate::memory::{alloc_pkt, memset, Dma, Mempool, Packet};
use crate::NicDevice;
use crate::{constants::*, hal::IxgbeHal};
use crate::{IxgbeError, IxgbeResult};
use alloc::sync::Arc;
use alloc::{collections::VecDeque, vec::Vec};
use core::marker::PhantomData;
use core::time::Duration;
use core::{mem, ptr};

const DRIVER_NAME: &str = "ixgbe";

const MAX_QUEUES: u16 = 64;

const PKT_BUF_ENTRY_SIZE: usize = 2048;
const MIN_MEMPOOL_SIZE: usize = 4096;

const NUM_RX_QUEUE_ENTRIES: usize = 512;
const NUM_TX_QUEUE_ENTRIES: usize = 512;
const TX_CLEAN_BATCH: usize = 32;

fn wrap_ring(index: usize, ring_size: usize) -> usize {
    (index + 1) & (ring_size - 1)
}

/// Ixgbe device.
pub struct IxgbeDevice<H: IxgbeHal> {
    // pci_addr: String,
    addr: *mut u8,
    len: usize,
    num_rx_queues: u16,
    num_tx_queues: u16,
    rx_queues: Vec<IxgbeRxQueue<H>>,
    tx_queues: Vec<IxgbeTxQueue<H>>,
    interrupts: Interrupts,
    _marker: PhantomData<H>,
}

struct IxgbeRxQueue<H: IxgbeHal> {
    descriptors: *mut ixgbe_adv_rx_desc,
    num_descriptors: usize,
    pool: Arc<Mempool<H>>,
    bufs_in_use: Vec<usize>,
    rx_index: usize,
}

impl<H: IxgbeHal> IxgbeRxQueue<H> {
    fn can_recv(&self) -> bool {
        let rx_index = self.rx_index;

        let desc = unsafe { self.descriptors.add(rx_index) as *mut ixgbe_adv_rx_desc };
        let status = unsafe { ptr::read_volatile(&mut (*desc).wb.upper.status_error as *mut u32) };

        status & IXGBE_RXDADV_STAT_DD != 0
    }
}

struct IxgbeTxQueue<H: IxgbeHal> {
    descriptors: *mut ixgbe_adv_tx_desc,
    num_descriptors: usize,
    pool: Option<Arc<Mempool<H>>>,
    bufs_in_use: VecDeque<usize>,
    clean_index: usize,
    tx_index: usize,
}

/// A buffer used for receiving.
pub struct RxBuffer<H: IxgbeHal> {
    packet: Packet<H>,
}

impl<H: IxgbeHal> RxBuffer<H> {
    /// Returns packets
    pub fn packet(&self) -> &Packet<H> {
        &self.packet
    }
}

/// A buffer used for transmitting.
pub struct TxBuffer<H: IxgbeHal> {
    packet: Packet<H>,
}

impl<H: IxgbeHal> TxBuffer<H> {
    /// Allocate a packet based on [`Mempool`].
    pub fn alloc(pool: &Arc<Mempool<H>>, size: usize) -> IxgbeResult<Self> {
        if let Some(pkt) = alloc_pkt(pool, size) {
            Ok(TxBuffer { packet: pkt })
        } else {
            Err(IxgbeError::NoMemory)
        }
    }

    /// Returns an unmutuable packet buffer.
    pub fn packet(&self) -> &[u8] {
        self.packet.as_bytes()
    }

    /// Returns a mutuable packet buffer.
    pub fn packet_mut(&mut self) -> &mut [u8] {
        self.packet.as_mut_bytes()
    }
}

impl<H: IxgbeHal> NicDevice<H> for IxgbeDevice<H> {
    fn get_driver_name(&self) -> &str {
        DRIVER_NAME
    }

    /// Returns the link speed of this device.
    fn get_link_speed(&self) -> u16 {
        let speed = self.get_reg32(IXGBE_LINKS);
        if (speed & IXGBE_LINKS_UP) == 0 {
            return 0;
        }
        match speed & IXGBE_LINKS_SPEED_82599 {
            IXGBE_LINKS_SPEED_100_82599 => 100,
            IXGBE_LINKS_SPEED_1G_82599 => 1000,
            IXGBE_LINKS_SPEED_10G_82599 => 10000,
            _ => 0,
        }
    }

    /// Returns the mac address of this device.
    fn get_mac_addr(&self) -> [u8; 6] {
        let low = self.get_reg32(IXGBE_RAL(0));
        let high = self.get_reg32(IXGBE_RAH(0));

        [
            (low & 0xff) as u8,
            (low >> 8 & 0xff) as u8,
            (low >> 16 & 0xff) as u8,
            (low >> 24) as u8,
            (high & 0xff) as u8,
            (high >> 8 & 0xff) as u8,
        ]
    }

    fn get_pci_addr(&self) -> &str {
        todo!()
    }

    /// Reads the stats of this device into `stats`.
    fn read_stats(&self, stats: &mut crate::DeviceStats) {
        let rx_pkts = u64::from(self.get_reg32(IXGBE_GPRC));
        let tx_pkts = u64::from(self.get_reg32(IXGBE_GPTC));
        let rx_bytes =
            u64::from(self.get_reg32(IXGBE_GORCL)) + (u64::from(self.get_reg32(IXGBE_GORCH)) << 32);
        let tx_bytes =
            u64::from(self.get_reg32(IXGBE_GOTCL)) + (u64::from(self.get_reg32(IXGBE_GOTCH)) << 32);

        stats.rx_pkts += rx_pkts;
        stats.tx_pkts += tx_pkts;
        stats.rx_bytes += rx_bytes;
        stats.tx_bytes += tx_bytes;
    }

    /// Resets the stats of this device.
    fn reset_stats(&mut self) {
        self.get_reg32(IXGBE_GPRC);
        self.get_reg32(IXGBE_GPTC);
        self.get_reg32(IXGBE_GORCL);
        self.get_reg32(IXGBE_GORCH);
        self.get_reg32(IXGBE_GOTCL);
        self.get_reg32(IXGBE_GOTCH);
    }

    /// Sets the mac address of this device.
    fn set_mac_addr(&self, mac: [u8; 6]) {
        let low: u32 = u32::from(mac[0])
            + (u32::from(mac[1]) << 8)
            + (u32::from(mac[2]) << 16)
            + (u32::from(mac[3]) << 24);
        let high: u32 = u32::from(mac[4]) + (u32::from(mac[5]) << 8);

        self.set_reg32(IXGBE_RAL(0), low);
        self.set_reg32(IXGBE_RAH(0), high);
    }

    /// Receives a [`RxBuffer`] from network. If currently no data, returns an error
    /// with type [`IxgbeError::NotReady`].
    ///
    /// It will try to pop a buffer that completed data reception in the NIC queue.
    fn receive(&mut self, queue_id: u16) -> IxgbeResult<RxBuffer<H>> {
        // 1. Get received queue.

        let queue = self
            .rx_queues
            .get_mut(queue_id as usize)
            .ok_or(IxgbeError::InvalidQueue)?;

        // Can't receive, return [`IxgbeError::NotReady`]
        if !queue.can_recv() {
            info!("Can not receive packets.");
            return Err(IxgbeError::NotReady);
        }

        info!("Queue {} has packets", queue_id);
        let mut rx_index = queue.rx_index;
        let last_rx_index = queue.rx_index;

        // 2. pop a packet from the queue.

        // Read ixgbe descriptor.
        let desc = unsafe { queue.descriptors.add(rx_index) as *mut ixgbe_adv_rx_desc };
        let status = unsafe { ptr::read_volatile(&mut (*desc).wb.upper.status_error as *mut u32) };

        if (status & IXGBE_RXDADV_STAT_EOP) == 0 {
            panic!("increase buffer size or decrease MTU")
        }

        let pool = &queue.pool;
        // Get a free buffer from the mempool
        if let Some(buf) = pool.alloc_buf() {
            // replace currently used buffer with new buffer.
            let buf = mem::replace(&mut queue.bufs_in_use[rx_index], buf);

            let p = Packet::<H> {
                addr_virt: pool.get_virt_addr(buf),
                addr_phys: pool.get_phys_addr(buf),
                len: unsafe { ptr::read_volatile(&(*desc).wb.upper.length as *const u16) as usize },
                pool: pool.clone(),
                pool_entry: buf,
                _marker: PhantomData,
            };

            unsafe {
                ptr::write_volatile(
                    &mut (*desc).read.pkt_addr as *mut u64,
                    pool.get_phys_addr(queue.bufs_in_use[rx_index]) as u64,
                );
                ptr::write_volatile(&mut (*desc).read.hdr_addr as *mut u64, 0);
            }

            rx_index = wrap_ring(rx_index, queue.num_descriptors);

            self.set_reg32(IXGBE_VFRDT(u32::from(queue_id)), last_rx_index as u32);
            self.rx_queues[queue_id as usize].rx_index = rx_index;

            return Ok(RxBuffer { packet: p });
        }
        Err(IxgbeError::NoMemory)
    }

    /// Sends a [`TxBuffer`] to the network. If currently queue is full, returns an
    /// error with type [`IxgbeError::QueueFull`].
    fn send(&mut self, queue_id: u16, tx_buf: TxBuffer<H>) -> IxgbeResult {
        let queue = self
            .tx_queues
            .get_mut(queue_id as usize)
            .ok_or(IxgbeError::InvalidQueue)?;

        let cur_index = queue.tx_index;
        let clean_index = clean_tx_queue(queue);

        let packet = tx_buf.packet;

        if queue.pool.is_some() {
            if !Arc::ptr_eq(queue.pool.as_ref().unwrap(), &packet.pool) {
                queue.pool = Some(packet.pool.clone());
            }
        } else {
            queue.pool = Some(packet.pool.clone());
        }

        assert!(
            Arc::ptr_eq(queue.pool.as_ref().unwrap(), &packet.pool),
            "Distince memory pools for a single tx queue are not supported yet."
        );

        let next_index = wrap_ring(cur_index, queue.num_descriptors);

        if clean_index == next_index {
            info!("Queue {} is full", queue_id);
            return Err(IxgbeError::QueueFull);
        }

        queue.tx_index = wrap_ring(queue.tx_index, queue.num_descriptors);

        unsafe {
            ptr::write_volatile(
                &mut (*queue.descriptors.add(cur_index)).read.buffer_addr as *mut u64,
                packet.get_phys_addr() as u64,
            );

            ptr::write_volatile(
                &mut (*queue.descriptors.add(cur_index)).read.cmd_type_len as *mut u32,
                IXGBE_ADVTXD_DCMD_EOP
                    | IXGBE_ADVTXD_DCMD_RS
                    | IXGBE_ADVTXD_DCMD_IFCS
                    | IXGBE_ADVTXD_DCMD_DEXT
                    | IXGBE_ADVTXD_DTYP_DATA
                    | packet.len() as u32,
            );

            ptr::write_volatile(
                &mut (*queue.descriptors.add(cur_index)).read.olinfo_status as *mut u32,
                (packet.len() as u32) << IXGBE_ADVTXD_PAYLEN_SHIFT,
            );
        }

        queue.bufs_in_use.push_back(packet.pool_entry);
        mem::forget(packet);

        self.set_reg32(
            IXGBE_TDT(u32::from(queue_id)),
            self.tx_queues[queue_id as usize].tx_index as u32,
        );

        Ok(())
    }
}

impl<H: IxgbeHal> IxgbeDevice<H> {
    /// Returns an initialized `IxgbeDevice` on success.
    ///
    /// # Panics
    /// Panics if `num_rx_queues` or `num_tx_queues` exceeds `MAX_QUEUES`.
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
            interrupts: Interrupts::default(),
            _marker: PhantomData,
        };
        dev.reset_and_init()?;
        Ok(dev)
    }

    /// Returns the number of receive queues.
    pub fn num_rx_queues(&self) -> u16 {
        self.num_rx_queues
    }

    /// Returns the number of transmit queues.
    pub fn num_tx_queues(&self) -> u16 {
        self.num_tx_queues
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
        let _ = H::wait_until(Duration::from_millis(1000));

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

        // skip last step from 4.6.3 - we don't want interrupts(ixy)
        // section 4.6.3 Enable interrupts.

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

        // enable promisc mode by default to make testing easier
        self.set_promisc(true);

        // wait some time for the link to come up
        self.wait_for_link();

        info!("Success to initialize and reset Intel 10G NIC regs.");

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
            info!("initializing rx queue {}", i);
            // enable advanced rx descriptors
            self.set_reg32(
                IXGBE_SRRCTL(u32::from(i)),
                (self.get_reg32(IXGBE_SRRCTL(u32::from(i))) & !IXGBE_SRRCTL_DESCTYPE_MASK)
                    | IXGBE_SRRCTL_DESCTYPE_ADV_ONEBUF,
            );
            // let nic drop packets if no rx descriptor is available instead of buffering them
            self.set_flags32(IXGBE_SRRCTL(u32::from(i)), IXGBE_SRRCTL_DROP_EN);

            // section 7.1.9 - setup descriptor ring
            let ring_size_bytes = NUM_RX_QUEUE_ENTRIES * mem::size_of::<ixgbe_adv_rx_desc>();

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

            info!("rx ring {} phys addr: {:#x}", i, dma.phys);
            info!("rx ring {} virt addr: {:p}", i, dma.virt);

            // set ring to empty at start
            self.set_reg32(IXGBE_RDH(u32::from(i)), 0);
            self.set_reg32(IXGBE_RDT(u32::from(i)), 0);

            let mempool_size = if NUM_RX_QUEUE_ENTRIES + NUM_TX_QUEUE_ENTRIES < MIN_MEMPOOL_SIZE {
                MIN_MEMPOOL_SIZE
            } else {
                NUM_RX_QUEUE_ENTRIES + NUM_TX_QUEUE_ENTRIES
            };

            let mempool = Mempool::allocate(mempool_size, PKT_BUF_ENTRY_SIZE).unwrap();

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
            info!("initializing tx queue {}", i);
            // section 7.1.9 - setup descriptor ring
            let ring_size_bytes = NUM_TX_QUEUE_ENTRIES * mem::size_of::<ixgbe_adv_tx_desc>();

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

            info!("tx ring {} phys addr: {:#x}", i, dma.phys);
            info!("tx ring {} virt addr: {:p}", i, dma.virt);

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

                let id = match pool.alloc_buf() {
                    Some(x) => x,
                    None => return Err(IxgbeError::NoMemory),
                };

                unsafe {
                    ptr::write_volatile(
                        &mut (*queue.descriptors.add(i)).read.pkt_addr as *mut u64,
                        pool.get_phys_addr(id) as u64,
                    );

                    ptr::write_volatile(
                        &mut (*queue.descriptors.add(i)).read.hdr_addr as *mut u64,
                        0,
                    );
                }

                // we need to remember which descriptor entry belongs to which mempool entry
                queue.bufs_in_use.push(id);
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

    /// Disable interrupt for queue with `queue_id`.
    fn disable_interrupt(&self, queue_id: u16) {
        // Clear interrupt mask to stop from interrupts being generated
        let mut mask: u32 = self.get_reg32(IXGBE_EIMS);
        mask &= !(1 << queue_id);
        self.set_reg32(IXGBE_EIMS, mask);
        self.clear_interrupt(queue_id);
        debug!("Using polling");
    }

    /// Clear interrupt for queue with `queue_id`.
    fn clear_interrupt(&self, queue_id: u16) {
        // Clear interrupt mask
        self.set_reg32(IXGBE_EIMC, 1 << queue_id);
        self.get_reg32(IXGBE_EICR);
    }

    /// Clear all interrupt masks for all queues.
    fn clear_interrupts(&self) {
        // Clear interrupt mask
        self.set_reg32(IXGBE_EIMC, IXGBE_IRQ_CLEAR_MASK);
        self.get_reg32(IXGBE_EICR);
    }

    /// Enable MSI interrupt for queue with `queue_id`.
    fn enable_msi_interrupt(&self, queue_id: u16) {
        // Step 1: The software driver associates between Tx and Rx interrupt causes and the EICR
        // register by setting the IVAR[n] registers.
        self.set_ivar(0, queue_id, 0);

        // Step 2: Program SRRCTL[n].RDMTS (per receive queue) if software uses the receive
        // descriptor minimum threshold interrupt
        // We don't use the minimum threshold interrupt

        // Step 3: All interrupts should be set to 0b (no auto clear in the EIAC register). Following an
        // interrupt, software might read the EICR register to check for the interrupt causes.
        self.set_reg32(IXGBE_EIAC, 0x0000_0000);

        // Step 4: Set the auto mask in the EIAM register according to the preferred mode of operation.
        // In our case we prefer to not auto-mask the interrupts

        // Step 5: Set the interrupt throttling in EITR[n] and GPIE according to the preferred mode of operation.
        self.set_reg32(IXGBE_EITR(u32::from(queue_id)), self.interrupts.itr_rate);

        // Step 6: Software clears EICR by writing all ones to clear old interrupt causes
        self.clear_interrupts();

        // Step 7: Software enables the required interrupt causes by setting the EIMS register
        let mut mask: u32 = self.get_reg32(IXGBE_EIMS);
        mask |= 1 << queue_id;
        self.set_reg32(IXGBE_EIMS, mask);
        debug!("Using MSI interrupts");
    }

    /// Enable MSI-X interrupt for queue with `queue_id`.
    fn enable_msix_interrupt(&self, queue_id: u16) {
        // Step 1: The software driver associates between interrupt causes and MSI-X vectors and the
        //throttling timers EITR[n] by programming the IVAR[n] and IVAR_MISC registers.
        let mut gpie: u32 = self.get_reg32(IXGBE_GPIE);
        gpie |= IXGBE_GPIE_MSIX_MODE | IXGBE_GPIE_PBA_SUPPORT | IXGBE_GPIE_EIAME;
        self.set_reg32(IXGBE_GPIE, gpie);
        self.set_ivar(0, queue_id, u32::from(queue_id));

        // Step 2: Program SRRCTL[n].RDMTS (per receive queue) if software uses the receive
        // descriptor minimum threshold interrupt
        // We don't use the minimum threshold interrupt

        // Step 3: The EIAC[n] registers should be set to auto clear for transmit and receive interrupt
        // causes (for best performance). The EIAC bits that control the other and TCP timer
        // interrupt causes should be set to 0b (no auto clear).
        self.set_reg32(IXGBE_EIAC, IXGBE_EIMS_RTX_QUEUE);

        // Step 4: Set the auto mask in the EIAM register according to the preferred mode of operation.
        // In our case we prefer to not auto-mask the interrupts

        // Step 5: Set the interrupt throttling in EITR[n] and GPIE according to the preferred mode of operation.
        // 0x000 (0us) => ... INT/s
        // 0x008 (2us) => 488200 INT/s
        // 0x010 (4us) => 244000 INT/s
        // 0x028 (10us) => 97600 INT/s
        // 0x0C8 (50us) => 20000 INT/s
        // 0x190 (100us) => 9766 INT/s
        // 0x320 (200us) => 4880 INT/s
        // 0x4B0 (300us) => 3255 INT/s
        // 0x640 (400us) => 2441 INT/s
        // 0x7D0 (500us) => 2000 INT/s
        // 0x960 (600us) => 1630 INT/s
        // 0xAF0 (700us) => 1400 INT/s
        // 0xC80 (800us) => 1220 INT/s
        // 0xE10 (900us) => 1080 INT/s
        // 0xFA7 (1000us) => 980 INT/s
        // 0xFFF (1024us) => 950 INT/s
        self.set_reg32(IXGBE_EITR(u32::from(queue_id)), self.interrupts.itr_rate);

        // Step 6: Software enables the required interrupt causes by setting the EIMS register
        let mut mask: u32 = self.get_reg32(IXGBE_EIMS);
        mask |= 1 << queue_id;
        self.set_reg32(IXGBE_EIMS, mask);
        debug!("Using MSIX interrupts");
    }

    /// Waits for the link to come up.
    fn wait_for_link(&self) {
        info!("waiting for link");
        // get the current time in seconds
        let time = unsafe { core::arch::x86_64::_rdtsc() / H::get_tsc_frequency() };
        let mut speed = self.get_link_speed();
        while speed == 0
            && unsafe { core::arch::x86_64::_rdtsc() / H::get_tsc_frequency() - time } < 10
        {
            // thread::sleep(Duration::from_millis(100));
            core::hint::spin_loop();
            // let _ = H::wait_ms(100);
            speed = self.get_link_speed();
        }
        info!("link speed is {} Mbit/s", self.get_link_speed());
    }

    /// Enables or disables promisc mode of this device.
    fn set_promisc(&self, enabled: bool) {
        if enabled {
            info!("enabling promisc mode");
            self.set_flags32(IXGBE_FCTRL, IXGBE_FCTRL_MPE | IXGBE_FCTRL_UPE);
        } else {
            info!("disabling promisc mode");
            self.clear_flags32(IXGBE_FCTRL, IXGBE_FCTRL_MPE | IXGBE_FCTRL_UPE);
        }
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
            // `thread::sleep(Duration::from_millis(100));`
            // let _ = H::wait_ms(100);
            let _ = H::wait_until(Duration::from_millis(100));
        }
    }

    /// Waits for `self.addr` + `reg` to set `value`.
    fn wait_set_reg32(&self, reg: u32, value: u32) {
        loop {
            let current = self.get_reg32(reg);
            if (current & value) == value {
                break;
            }
            // `thread::sleep(Duration::from_millis(100));`
            // let _ = H::wait_ms(100);
            let _ = H::wait_until(Duration::from_millis(100));
        }
    }

    /// Maps interrupt causes to vectors by specifying the `direction` (0 for Rx, 1 for Tx),
    /// the `queue` ID and the corresponding `misx_vector`.
    fn set_ivar(&self, direction: u32, queue: u16, mut msix_vector: u32) {
        let mut ivar: u32;
        // let index: u32;
        msix_vector |= IXGBE_IVAR_ALLOC_VAL;
        let index = 16 * (u32::from(queue) & 1) + 8 * direction;
        ivar = self.get_reg32(IXGBE_IVAR(u32::from(queue) >> 1));
        ivar &= !(0xFF << index);
        ivar |= msix_vector << index;
        self.set_reg32(IXGBE_IVAR(u32::from(queue) >> 1), ivar);
    }
}

/// Removes multiples of `TX_CLEAN_BATCH` packets from `queue`.
fn clean_tx_queue<H: IxgbeHal>(queue: &mut IxgbeTxQueue<H>) -> usize {
    let mut clean_index = queue.clean_index;
    let cur_index = queue.tx_index;

    loop {
        let mut cleanable = cur_index as i32 - clean_index as i32;

        if cleanable < 0 {
            cleanable += queue.num_descriptors as i32;
        }

        if cleanable < TX_CLEAN_BATCH as i32 {
            break;
        }

        let mut cleanup_to = clean_index + TX_CLEAN_BATCH - 1;

        if cleanup_to >= queue.num_descriptors {
            cleanup_to -= queue.num_descriptors;
        }

        let status = unsafe {
            ptr::read_volatile(&(*queue.descriptors.add(cleanup_to)).wb.status as *const u32)
        };

        if (status & IXGBE_ADVTXD_STAT_DD) != 0 {
            if let Some(ref p) = queue.pool {
                if TX_CLEAN_BATCH >= queue.bufs_in_use.len() {
                    p.free_stack
                        .borrow_mut()
                        .extend(queue.bufs_in_use.drain(..))
                } else {
                    p.free_stack
                        .borrow_mut()
                        .extend(queue.bufs_in_use.drain(..TX_CLEAN_BATCH))
                }
            }

            clean_index = wrap_ring(cleanup_to, queue.num_descriptors);
        } else {
            break;
        }
    }

    queue.clean_index = clean_index;

    clean_index
}

unsafe impl<H: IxgbeHal> Sync for IxgbeDevice<H> {}
unsafe impl<H: IxgbeHal> Send for IxgbeDevice<H> {}
