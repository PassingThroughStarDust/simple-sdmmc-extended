use core::{
    ptr::NonNull,
    sync::atomic::{Ordering, compiler_fence},
};

use log::{debug, info, trace};
use volatile::VolatilePtr;

use crate::{
    cmd::{Command, DataXfer},
    regs::{ClkDiv, ClkEna, RegisterBlock, RegisterBlockVolatileFieldAccess},
    utils::{Cid, CsdV2},
};

const IDMAC_INT_TI: u32 = 1;
const IDMAC_INT_RI: u32 = 1 << 1;
const IDMAC_INT_FBE: u32 = 1 << 2;
const IDMAC_INT_DU: u32 = 1 << 4;
const IDMAC_INT_CES: u32 = 1 << 5;
const IDMAC_INT_COMPLETE_MASK: u32 = IDMAC_INT_TI | IDMAC_INT_RI;
const IDMAC_INT_ERROR_MASK: u32 = IDMAC_INT_FBE | IDMAC_INT_DU | IDMAC_INT_CES;

#[repr(C, align(64))]
struct IdmacDesc {
    des0: u32,
    des1: u32,
    des2: u32,
    des3: u32,
}

impl IdmacDesc {
    const OWN: u32 = 1 << 31;
    const FIRST: u32 = 1 << 3;
    const LAST: u32 = 1 << 2;

    const fn new() -> Self {
        Self {
            des0: 0,
            des1: 0,
            des2: 0,
            des3: 0,
        }
    }

    fn configure(&mut self, buffer: *const u8, len: usize) {
        assert!(len > 0, "DMA transfer length must be non-zero");
        assert!(
            len <= 0x1fff,
            "DMA transfer length exceeds descriptor capacity"
        );
        let addr = buffer as usize as u64;
        assert!(
            addr <= u32::MAX as u64,
            "buffer is outside the 32-bit address space"
        );

        self.des0 = Self::OWN | Self::FIRST | Self::LAST;
        self.des1 = len as u32;
        self.des2 = addr as u32;
        self.des3 = 0;
    }

    fn descriptor_address(&self) -> u32 {
        let addr = self as *const _ as usize as u64;
        assert!(addr <= u32::MAX as u64, "descriptor outside 32-bit range");
        addr as u32
    }
}

struct DmaState {
    desc: IdmacDesc,
    active: bool,
    last_status: u32,
}

impl DmaState {
    const fn new() -> Self {
        Self {
            desc: IdmacDesc::new(),
            active: false,
            last_status: 0,
        }
    }
}

fn wait_until<F>(mut f: F)
where
    F: FnMut() -> bool,
{
    // TODO: yield?
    while !f() {
        core::hint::spin_loop();
    }
}

/// SD/MMC driver.
pub struct SdMmc {
    regs: VolatilePtr<'static, RegisterBlock>,
    num_blocks: u64,
    dma: DmaState,
}

impl SdMmc {
    const FIFO: usize = 0x200;

    /// Creates a new `SdMmc` instance from the given base address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `base` is a valid pointer to the SD/MMC controller's
    /// register block and that no other code is concurrently accessing the same hardware.
    pub unsafe fn new(base: usize) -> Self {
        let regs = unsafe { VolatilePtr::new(NonNull::new_unchecked(base as *mut _)) };

        let mut this = Self {
            regs,
            num_blocks: 0,
            dma: DmaState::new(),
        };
        this.init();
        this
    }

    fn can_send_cmd(&self) -> bool {
        !self.regs.cmd().read().start_cmd()
    }

    fn can_send_data(&self) -> bool {
        !self.regs.status().read().data_busy()
    }

    fn has_response(&self) -> bool {
        self.regs.rintsts().read().command_done()
    }

    fn fifo_cnt(&self) -> usize {
        self.regs.status().read().fifo_count() as usize
    }

    fn set_transaction_size(&self, blk_size: u16, byte_cnt: u32) {
        self.regs.blksiz().update(|r| r.with_block_size(blk_size));
        self.regs.bytcnt().write(byte_cnt);
    }

    fn prepare_dma_transfer(&mut self, buffer: *const u8, len: usize) {
        debug_assert!(!self.dma.active, "DMA transfer already active");
        self.dma.desc.configure(buffer, len);
        self.dma.last_status = 0;
        compiler_fence(Ordering::SeqCst);

        self.regs.idsts().write(u32::MAX);
        self.regs.idinten().write(0);

        let desc_addr = self.dma.desc.descriptor_address();
        self.regs.dbaddr().write(desc_addr);

        self.regs
            .ctrl()
            .update(|r| r.with_use_internal_dmac(true).with_dma_enable(true));

        self.regs
            .bmod()
            .update(|r| r.with_swr(false).with_fb(true).with_de(true));

        self.regs.pldmnd().write(1);
        self.dma.active = true;
    }

    fn wait_for_dma_completion(&mut self) -> bool {
        debug_assert!(self.dma.active, "No DMA transfer pending");
        wait_until(|| {
            let status = self.regs.idsts().read();
            self.dma.last_status = status;
            status & (IDMAC_INT_COMPLETE_MASK | IDMAC_INT_ERROR_MASK) != 0
        });

        let status = self.dma.last_status;
        self.regs.idsts().write(status);

        wait_until(|| {
            let rintsts = self.regs.rintsts().read();
            rintsts.data_transfer_over() || rintsts.error()
        });

        self.regs.ctrl().update(|r| r.with_dma_enable(false));
        self.dma.active = false;

        (status & IDMAC_INT_ERROR_MASK) == 0
    }

    fn send_cmd(&mut self, command: Command<'_>) -> Option<[u32; 4]> {
        trace!("send_cmd {command:#x?}");

        let (cmd, arg, xfer) = command.build();
        assert_eq!(cmd.data_expected(), xfer.is_some());

        let mut dma_failed = false;

        trace!("send_cmd {cmd:?} {arg:#x?}");

        wait_until(|| self.can_send_cmd());
        if cmd.data_expected() {
            wait_until(|| self.can_send_data());
        }

        self.regs.cmdarg().write(arg);
        self.regs.cmd().write(cmd);

        wait_until(|| self.can_send_cmd());
        trace!("cmd {} sent", cmd.cmd_index());

        if cmd.response_expect() {
            wait_until(|| self.has_response());
            trace!("cmd {} received response", cmd.cmd_index());
        }

        if let Some(xfer) = xfer {
            if self.dma.active {
                if !self.wait_for_dma_completion() {
                    dma_failed = true;
                }
            } else {
                let fifo_base =
                    unsafe { self.regs.as_raw_ptr().byte_add(Self::FIFO) }.cast::<u64>();
                let mut offset = 0;
                match xfer {
                    DataXfer::Read(buf) => {
                        wait_until(|| {
                            let rintsts = self.regs.rintsts().read();

                            if rintsts.receive_fifo_data_request() {
                                trace!("rxdr");
                                while self.fifo_cnt() >= 2 {
                                    let data =
                                        unsafe { fifo_base.byte_add(offset).read_volatile() };
                                    buf[offset..offset + 8].copy_from_slice(&data.to_le_bytes());
                                    offset += 8;
                                }
                            }

                            rintsts.data_transfer_over() || rintsts.error()
                        });
                        trace!("received {offset} bytes");
                    }
                    DataXfer::Write(buf) => {
                        wait_until(|| {
                            let rintsts = self.regs.rintsts().read();

                            if rintsts.transmit_fifo_data_request() {
                                trace!("txdr");
                                // Hard coded FIFO depth
                                while self.fifo_cnt() < 120 && offset < buf.len() {
                                    let data = u64::from_le_bytes(
                                        buf[offset..offset + 8].try_into().unwrap(),
                                    );
                                    unsafe { fifo_base.byte_add(offset).write_volatile(data) };
                                    offset += 8;
                                }
                            }

                            rintsts.data_transfer_over() || rintsts.error()
                        });
                        trace!("sent {offset} bytes");
                    }
                }
            }
        }

        let resp = self.regs.resp().read();

        let rintsts = self.regs.rintsts().read();
        // clear interrupt status
        self.regs.rintsts().write(rintsts);

        if dma_failed {
            trace!(
                "cmd {} DMA error: status {:#x}",
                cmd.cmd_index(),
                self.dma.last_status
            );
            return None;
        }

        if rintsts.error() {
            trace!("cmd {} error: {rintsts:?} resp: {resp:?}", cmd.cmd_index());
            return None;
        }
        Some(resp)
    }

    fn init(&mut self) {
        info!("Initializing SD/MMC driver at {:?}", self.regs);

        trace!("ctrl: {:?}", self.regs.ctrl().read());
        trace!("pwren: {:?}", self.regs.pwren().read());
        trace!("clkdiv: {:?}", self.regs.clkdiv().read());
        trace!("clksrc: {:?}", self.regs.clksrc().read());
        trace!("clkena: {:?}", self.regs.clkena().read());
        trace!("tmout: {:?}", self.regs.tmout().read());
        trace!("ctype: {:?}", self.regs.ctype().read());
        trace!("cdetect: {:?}", self.regs.cdetect().read());
        trace!("wrtprt: {:?}", self.regs.wrtprt().read());
        trace!("usrid: {:?}", self.regs.usrid().read());
        trace!("verid: {:?}", self.regs.verid().read());
        trace!("hcon: {:?}", self.regs.hcon().read());
        trace!("uhs: {:?}", self.regs.uhs().read());
        trace!("bmod: {:?}", self.regs.bmod().read());
        trace!("dbaddr: {:?}", self.regs.dbaddr().read());

        // reset clock
        self.regs.clkena().write(ClkEna::new());
        self.send_cmd(Command::ResetClock);

        // set clock divider to 400kHz (low)
        self.regs.clkdiv().write(ClkDiv::new().with_clk_divider0(4));

        // enable clock
        self.regs.clkena().write(ClkEna::new().with_cclk_enable(1));
        self.send_cmd(Command::ResetClock);

        trace!("clock reset");

        // set data width -> 1bit
        self.regs.ctype().write(0.into());

        // reset dma
        self.regs.bmod().update(|r| r.with_de(false).with_swr(true));
        self.regs.ctrl().update(|r| {
            r.with_dma_reset(true)
                .with_use_internal_dmac(true)
                .with_dma_enable(false)
        });

        trace!("dma reset");

        self.regs.idsts().write(u32::MAX);
        self.regs.idinten().write(0);
        self.regs
            .bmod()
            .update(|r| r.with_swr(false).with_fb(true).with_de(true));

        trace!("ctrl: {:?}", self.regs.ctrl().read());

        self.send_cmd(Command::GoIdleState);
        trace!("idle state set");

        let resp = self.send_cmd(Command::SendIfCond(0x1aa)).unwrap();
        assert_eq!(resp[0] & 0xff, 0xaa, "unsupported version");

        loop {
            self.send_cmd(Command::AppCmd(0));
            let resp = self.send_cmd(Command::SdSendOpCond(0x41FF_8000)).unwrap();
            let ocr = resp[0];
            if ocr & 0x8000_0000 != 0 {
                info!("SD card is ready");
                if ocr & 0x4000_0000 != 0 {
                    debug!("SD card supports high capacity");
                } else {
                    debug!("SD card is standard capacity");
                }
                break;
            }

            trace!("SD card not ready, ocr: {ocr:x}");
            core::hint::spin_loop();
        }

        let resp = self.send_cmd(Command::AllSendCid).unwrap();
        let cid = unsafe { core::mem::transmute::<[u32; 4], Cid>(resp) };
        info!("cid: {cid:?}");

        let resp = self.send_cmd(Command::SendRelativeAddr).unwrap();
        let rca = (resp[0] >> 16) & 0xffff;
        debug!("rca: {rca:#x}");

        let resp = self.send_cmd(Command::SendCsd(rca << 16)).unwrap();
        let csd = unsafe { core::mem::transmute::<[u32; 4], CsdV2>(resp) };
        debug!("csd: {csd:?}");

        self.num_blocks = csd.num_blocks();
        info!("SD card capacity: {:#x} blocks", self.num_blocks);

        self.send_cmd(Command::SelectCard(rca << 16)).unwrap();

        self.send_cmd(Command::AppCmd(rca << 16)).unwrap();

        self.set_transaction_size(8, 8);
        let mut buf = [0u8; 512];
        self.send_cmd(Command::SendScr(&mut buf)).unwrap();

        trace!("fifo count: {}", self.fifo_cnt());
        let resp = unsafe {
            self.regs
                .as_raw_ptr()
                .byte_add(Self::FIFO)
                .cast::<u64>()
                .read_volatile()
        };
        debug!("Bus width supported: {:#x?}", (resp >> 8) & 0xf);
        trace!("fifo count: {}", self.fifo_cnt());

        trace!("ctrl: {:?}", self.regs.ctrl().read());
        let rintsts = self.regs.rintsts().read();
        trace!("rintsts: {rintsts:?}");
        self.regs.rintsts().write(rintsts); // clear interrupt status

        info!("SD/MMC driver initialized");
    }

    /// Reads a single block from the SD/MMC card.
    pub fn read_block(&mut self, block: u32, buf: &mut [u8; 512]) {
        self.set_transaction_size(512, 512);
        self.prepare_dma_transfer(buf.as_mut_ptr() as *const u8, buf.len());
        self.send_cmd(Command::ReadSingleBlock(block, buf)).unwrap();
        trace!("fifo count: {}", self.fifo_cnt());
    }

    /// Writes a single block to the SD/MMC card.
    pub fn write_block(&mut self, block: u32, buf: &[u8; 512]) {
        self.set_transaction_size(512, 512);
        self.prepare_dma_transfer(buf.as_ptr(), buf.len());
        self.send_cmd(Command::WriteSingleBlock(block, buf))
            .unwrap();
        trace!("fifo count: {}", self.fifo_cnt());
    }

    /// Returns the number of blocks.
    pub fn num_blocks(&self) -> u64 {
        self.num_blocks
    }

    /// The size of a block in bytes.
    pub const BLOCK_SIZE: usize = 512;
}

unsafe impl Send for SdMmc {}
unsafe impl Sync for SdMmc {}
