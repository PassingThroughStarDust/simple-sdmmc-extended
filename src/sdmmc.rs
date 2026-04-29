use core::{
    ptr::NonNull,
    sync::atomic::{fence, Ordering},
};

use log::{debug, info, trace, warn};
use volatile::VolatilePtr;
use crate::dma::{
    IdmacDescriptor
};

use crate::{
    cmd::{Command, DataXfer},
    regs::{ClkDiv, ClkEna, RegisterBlock, RegisterBlockVolatileFieldAccess},
    utils::{Cid, CsdV2},
};

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
    /// Register block for the SD/MMC controller, accessed through volatile reads/writes.
    regs: VolatilePtr<'static, RegisterBlock>,

    /// Number of blocks on the SD/MMC card, determined during initialization from the CSD register.
    num_blocks: u64,

    dma_enabled: bool,

    // Indicates whether the SD/MMC controller supports 64-bit DMA addresses.
    // On VisionFive2, the DWC_MSHC is configured to operate in 32-bit addressing mode, so this is false.
    // support_dma_64bit_address: bool,

    // The size of the descriptor ring buffer, which is the number of descriptors in the ring.
    //dma_descriptor_ring_size: usize,

    // The virtual address of scatter-gather descriptors for DMA transfer.
    // This is accessed by CPU, and must be a valid pointer to memory where the descriptors are allocated.
    // sg_cpu: *mut IdmacDescriptor,

    // The physical address of scatter-gather descriptors for DMA transfer.
    // This is accessed by the SD/MMC controller's IDMAC, and must be bus-addressable.
    //sg_dma: *mut IdmacDescriptor,
}

impl SdMmc {
    /// The offset of the FIFO register from the base address of the SD/MMC controller's register block.
    const FIFO: usize = 0x200;

    // The size of the descriptor ring buffer, which is the number of descriptors in the ring.
    // Equal to page size (4096 bytes) divided by the size of each descriptor (16 bytes), resulting in 256 descriptors.
    // const DESC_RING_BUF_SZ: usize = 4096;

    /// The offset between the kernel's physical address space and virtual address space.
    /// This is used to convert between physical addresses (used for DMA) and virtual addresses (used by the CPU).
    const KERNEL_VIRT_PHYS_OFFSET: usize = 0xffff_ffc0_0000_0000;

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
            dma_enabled: false,
            // support_dma_64bit_address: false,
            // dma_descriptor_ring_size: Self::DESC_RING_BUF_SZ / core::mem::size_of::<IdmacDescriptor>(),
            // sg_cpu: core::ptr::null_mut(),
            // sg_dma: core::ptr::null_mut(),
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

    fn send_cmd(&self, command: Command<'_>) -> Option<[u32; 4]> {
        let is_reset_clock = matches!(command, Command::ResetClock);
        let is_go_idle = matches!(command, Command::GoIdleState);
        
        if is_reset_clock {
            info!(">>> Sending ResetClock command");
        }
        if is_go_idle {
            info!(">>> Sending GoIdleState command");
        }
        trace!("send_cmd {command:#x?}");

        let (cmd, arg, xfer) = command.build();
        assert_eq!(cmd.data_expected(), xfer.is_some());

        if is_reset_clock {
            info!("    ResetClock: update_clock_registers_only, response_expect={}", cmd.response_expect());
        }
        if is_go_idle {
            info!("    cmd: {:?}", cmd);
            info!("    response_expect: {}", cmd.response_expect());
            info!("    send_initialization: {}", cmd.send_initialization());
        }
        trace!("send_cmd {cmd:?} {arg:#x?}");

        if is_reset_clock {
            info!("    waiting for can_send_cmd...");
        }
        
        // Wait for command to be sendable (with timeout counter)
        let mut cmd_wait_count = 0u64;
        let cmd_max_wait = 1_000_000u64;  // ~1M iterations = few seconds on modern CPU
        while !self.can_send_cmd() {
            core::hint::spin_loop();
            cmd_wait_count += 1;
            if cmd_wait_count > cmd_max_wait {
                if is_go_idle {
                    warn!("    can_send_cmd timeout after {} iterations", cmd_wait_count);
                }
                break;
            }
        }
        if is_reset_clock {
            info!("    can_send_cmd: true (waited {} iterations)", cmd_wait_count);
        }
        if is_go_idle {
            info!("    can_send_cmd: true (waited {} iterations)", cmd_wait_count);
        }
        
        if cmd.data_expected() {
            let mut data_wait_count = 0u64;
            while !self.can_send_data() {
                core::hint::spin_loop();
                data_wait_count += 1;
            }
            if data_wait_count > 1000 && is_reset_clock {
                info!("    can_send_data: true (waited {} iterations)", data_wait_count);
            }
        }

        if is_go_idle {
            info!("    can_send_cmd: true");
        }
        self.regs.cmdarg().write(arg);
        self.regs.cmd().write(cmd);

        if is_reset_clock {
            info!("    wrote cmd register, cmd={:?}", cmd);
        }
        if is_go_idle {
            info!("    wrote cmd register");
            info!("    cmd register after write: {:?}", self.regs.cmd().read());
        }

        if is_reset_clock {
            info!("    waiting for start_cmd to clear...");
        }
        
        // Wait for command to complete (with timeout counter)
        let mut start_cmd_wait_count = 0u64;
        while !self.can_send_cmd() {
            core::hint::spin_loop();
            start_cmd_wait_count += 1;
            if start_cmd_wait_count > cmd_max_wait {
                if is_go_idle {
                    warn!("    start_cmd clear timeout after {} iterations", start_cmd_wait_count);
                }
                break;
            }
        }
        trace!("cmd {} sent", cmd.cmd_index());
        if is_reset_clock {
            info!("    start_cmd cleared (waited {} iterations)", start_cmd_wait_count);
        }

        if cmd.response_expect() {
            if is_go_idle {
                info!("    waiting for response...");
                let status_before = self.regs.status().read();
                let rintsts_before = self.regs.rintsts().read();
                info!("    Status before wait: {:?}", status_before);
                info!("    RINTSTS before wait: {:?}", rintsts_before);
            }
            
            // Wait for response (with timeout counter)
            let mut resp_wait_count = 0u64;
            while !self.has_response() {
                core::hint::spin_loop();
                resp_wait_count += 1;
                if resp_wait_count > cmd_max_wait {
                    if is_go_idle {
                        warn!("    response timeout after {} iterations", resp_wait_count);
                        let status_timeout = self.regs.status().read();
                        let rintsts_timeout = self.regs.rintsts().read();
                        warn!("    Status at timeout: {:?}", status_timeout);
                        warn!("    RINTSTS at timeout: {:?}", rintsts_timeout);
                    }
                    break;
                }
            }
            
            trace!("cmd {} received response", cmd.cmd_index());
            if is_go_idle {
                info!("    received response (waited {} iterations)", resp_wait_count);
            }
        } else {
            if is_reset_clock {
                info!("    no response expected for this command");
            }
        }

        if let Some(xfer) = xfer {
            let fifo_base = unsafe { self.regs.as_raw_ptr().byte_add(Self::FIFO) }.cast::<u64>();
            let mut offset = 0;
            match xfer {
                DataXfer::Read(buf) => {
                    wait_until(|| {
                        let rintsts = self.regs.rintsts().read();

                        if rintsts.receive_fifo_data_request() {
                            trace!("rxdr");
                            while self.fifo_cnt() >= 2 {
                                let data = unsafe { fifo_base.byte_add(offset).read_volatile() };
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
                                let data =
                                    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap());
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

        let resp = self.regs.resp().read();

        let rintsts = self.regs.rintsts().read();
        // clear interrupt status
        self.regs.rintsts().write(rintsts);

        if rintsts.error() {
            warn!("cmd {} error - rintsts: {rintsts:?}, resp: {resp:?}", cmd.cmd_index());
            warn!("  response_timeout: {}, data_read_timeout: {}, start_bit_error: {}, end_bit_error: {}",
                  rintsts.response_timeout(), rintsts.data_read_timeout(), rintsts.start_bit_error(), rintsts.end_bit_error());
            warn!("  data_crc_error: {}, response_crc_error: {}, response_error: {}, hardware_locked_write: {}",
                  rintsts.data_crc_error(), rintsts.response_crc_error(), rintsts.response_error(), rintsts.hardware_locked_write());
            return None;
        }
        Some(resp)
    }

    /// Sends a command using the Internal DMA (IDMAC) for data transfer if required.
    pub fn send_cmd_idmac(
        &self,
        command: Command<'_>,
    ) -> Option<[u32; 4]> {
        trace!("send_cmd_idmac {command:#x?}");

        let (cmd, arg, xfer) = command.build();
        assert_eq!(cmd.data_expected(), xfer.is_some());

        trace!("send_cmd_idmac {cmd:?} {arg:#x?}");

        wait_until(|| self.can_send_cmd());
        if cmd.data_expected() {
            wait_until(|| self.can_send_data());
        }

        // Clear stale status before a new command/DMA transaction.
        let stale_rintsts = self.regs.rintsts().read();
        self.regs.rintsts().write(stale_rintsts);
        let idsts_before = self.regs.idsts().read();

        if let Some(xfer) = xfer {
            info!("Data required, using IDMAC for transfer");

            let (buf_len, buf_ptr) = match xfer {
                DataXfer::Read(buf) => {
                    let len = buf.len();
                    (len, buf.as_ptr() as usize)
                }
                DataXfer::Write(buf) => {
                    let len = buf.len();
                    (len, buf.as_ptr() as usize)
                }
            };

            assert!(
                buf_len <= 0x1fff,
                "IDMAC single descriptor buffer too large: {buf_len}"
            );

            // Convert the virtual address of the buffer to a physical address for DMA.
            let buf_phys = buf_ptr - Self::KERNEL_VIRT_PHYS_OFFSET;
            trace!("Buffer physical address: 0x{:08x}", buf_phys);

            // Set up the IDMAC descriptor for the DMA transfer.
            // Use one descriptor for one contiguous buffer.
            let mut desc = IdmacDescriptor::new();
            // Set the control bits for the DMA transfer in des0.
            // OWN must be set so IDMAC can fetch and process the descriptor.
            desc.set_desc0_control_descriptor(true, false, false, false, true, true, false);
            desc.set_des1_buffer1_size(buf_len as u16);
            desc.set_des2_buffer1_address(buf_phys as u32);
            desc.set_des3_next_descriptor_address(0);

            // Write the physical address of the descriptor to the DBADDR register to set up the DMA transfer.
            let desc_addr = (&desc as *const IdmacDescriptor) as usize - Self::KERNEL_VIRT_PHYS_OFFSET;
            // Ensure descriptor writes are visible before giving its address to IDMAC.
            fence(Ordering::Release);
            self.regs.dbaddr().write(desc_addr as u32);
            self.regs.pldmnd().write(1);
            trace!("IDMAC descriptor set up at physical address: 0x{:08x}", desc_addr);
        }

        // Write the command argument and command index to the CMDARG and CMD registers to send the command.
        self.regs.cmdarg().write(arg);
        self.regs.cmd().write(cmd);

        trace!("cmd {} sent", cmd.cmd_index());

        // Wait for the command to be sent and the response to be received, checking for errors.
        if cmd.response_expect() {
            wait_until(|| self.has_response());
            trace!("cmd {} received response", cmd.cmd_index());
        }

        // If data transfer is expected, wait for the transfer to complete, checking for errors.
        if cmd.data_expected() {
            wait_until(|| {
                let rintsts = self.regs.rintsts().read();
                let idsts = self.regs.idsts().read();
                let idmac_new_error =
                    (!idsts_before.fbe() && idsts.fbe())
                        || (!idsts_before.du() && idsts.du())
                        || (!idsts_before.ces() && idsts.ces());

                rintsts.data_transfer_over()
                    || rintsts.error()
                    || idmac_new_error
            });
        }

        // Read response and check for errors after sending the command and setting up DMA.
        let resp = self.regs.resp().read();

        // Read the interrupt status to check for errors and clear the status bits.
        let rintsts = self.regs.rintsts().read();
        let idsts = self.regs.idsts().read();
        let idmac_new_error =
            (!idsts_before.fbe() && idsts.fbe())
                || (!idsts_before.du() && idsts.du())
                || (!idsts_before.ces() && idsts.ces());
        // clear interrupt status
        self.regs.rintsts().write(rintsts);

        if rintsts.error() || idmac_new_error {
            trace!(
                "cmd {} error: rintsts={rintsts:?} idsts={idsts:?} resp={resp:?}",
                cmd.cmd_index()
            );
            return None;
        }

        Some(resp)
    }

    fn init(&mut self) {
        info!("Initializing SD/MMC driver at {:?}", self.regs);

        // On VisionFive2, some registers have been initialized by the bootloader(U-Boot).
        // But some default values are not suitable for our driver, so we need to reset and reconfigure them.
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

        // Clear any stale interrupt status flags left by bootloader
        // Writing 1 to these bits clears them
        let rintsts = self.regs.rintsts().read();
        trace!("initial rintsts: {rintsts:?}");
        self.regs.rintsts().write(rintsts);
        trace!("cleared interrupt status");

        // Clock is already initialized by U-Boot, but we need to reconfigure it
        // First, check current state
        warn!("=== SD/MMC Clock Initialization ===");
        warn!("Initial clkena: {:?}", self.regs.clkena().read());
        warn!("Initial clkdiv: {:?}", self.regs.clkdiv().read());
        warn!("Initial ctrl: {:?}", self.regs.ctrl().read());

        // Disable clock for configuration
        warn!("Step 1: Disabling clock...");
        self.regs.clkena().write(ClkEna::new());
        warn!("  clkena after disable: {:?}", self.regs.clkena().read());
        
        // Send ResetClock command to update clock in disabled state
        warn!("Step 2: Sending ResetClock in disabled state...");
        match self.send_cmd(Command::ResetClock) {
            Some(_) => warn!("  ResetClock succeeded"),
            None => warn!("  ResetClock FAILED - continuing anyway"),
        }

        // Set clock divider to lower frequency (slower for compatibility)
        warn!("Step 3: Setting clock divider to 100 (lower frequency)...");
        self.regs.clkdiv().write(ClkDiv::new().with_clk_divider0(100));
        warn!("  clkdiv after set: {:?}", self.regs.clkdiv().read());

        // Now enable clock with new divider
        warn!("Step 4: Enabling clock...");
        self.regs.clkena().write(ClkEna::new().with_cclk_enable(1));
        warn!("  clkena after enable: {:?}", self.regs.clkena().read());
        
        // Send ResetClock to activate new clock settings
        warn!("Step 5: Sending ResetClock to activate new clock...");
        match self.send_cmd(Command::ResetClock) {
            Some(_) => warn!("  ResetClock succeeded"),
            None => warn!("  ResetClock FAILED - continuing anyway"),
        }
        
        // Long delay to let everything stabilize
        warn!("Step 6: Waiting for clock stabilization...");
        for _ in 0..10000 {
            core::hint::spin_loop();
        }
        
        warn!("Clock initialization complete:");
        warn!("  Final clkena: {:?}", self.regs.clkena().read());
        warn!("  Final clkdiv: {:?}", self.regs.clkdiv().read());
        warn!("  Final status: {:?}", self.regs.status().read());
        warn!("=== End Clock Initialization ===");

        // Check card presence and status
        warn!("=== Pre-Command Diagnostics ===");
        warn!("CTYPE (card type): {:?}", self.regs.ctype().read());
        warn!("STATUS register: {:?}", self.regs.status().read());
        let status = self.regs.status().read();
        warn!("  card_data_3_status: {}", status.data_3_status());
        warn!("  fifo_count: {}", status.fifo_count());
        warn!("  command_fsm_states: {}", status.command_fsm_states());
        warn!("RINTSTS (interrupt status): {:?}", self.regs.rintsts().read());
        warn!("MINTSTS (masked interrupt): {:?}", self.regs.mintsts().read());
        
        // Enable card power if available in PWREN register
        warn!("Setting card power enable (PWREN)...");
        self.regs.pwren().write(1u32.into());  // Card power enable
        warn!("  PWREN: {:?}", self.regs.pwren().read());
        
        // Increased stabilization delay
        warn!("Extended clock stabilization delay (100k cycles)...");
        for _ in 0..100000 {
            core::hint::spin_loop();
        }
        
        // set data width -> 1bit
        self.regs.ctype().write(0.into());

        // reset dma
        self.regs.bmod().update(|r| r.with_de(false).with_swr(true));
        self.regs
            .ctrl()
            .update(|r| r.with_dma_reset(true).with_use_internal_dmac(false));

        trace!("dma reset");

        trace!("ctrl: {:?}", self.regs.ctrl().read());

        warn!("=== Sending GoIdleState command ===");
        warn!("  Before GoIdleState - STATUS: {:?}", self.regs.status().read());
        warn!("  Before GoIdleState - RINTSTS: {:?}", self.regs.rintsts().read());
        // Note: GoIdleState may timeout during initial card detection phase.
        // This is not fatal - the card responds to SendIfCond and continues initialization normally.
        // The timeout likely occurs because the card is still stabilizing at the new clock frequency.
        match self.send_cmd(Command::GoIdleState) {
            Some(_) => warn!("GoIdleState succeeded"),
            None => warn!("GoIdleState timeout (expected during initialization) - continuing..."),
        }
        trace!("idle state set");

        warn!("Sending SendIfCond command...");
        let has_valid_resp = match self.send_cmd(Command::SendIfCond(0x1aa)) {
            Some(resp) => {
                warn!("SendIfCond succeeded: {:?}", resp);
                if resp[0] & 0xff != 0xaa {
                    warn!("Warning: unexpected response for SendIfCond");
                    false
                } else {
                    true
                }
            }
            None => {
                warn!("SendIfCond FAILED - card not responding or unsupported");
                false
            }
        };
        
        if !has_valid_resp {
            warn!("SD card not responding properly - continuing anyway");
        }

        warn!("Starting ACMD41 loop to detect SD card...");
        let mut attempt = 0;
        let mut card_initialized = false;
        loop {
            attempt += 1;
            if attempt > 100 {
                warn!("ACMD41 loop exceeded 100 attempts - giving up");
                break;
            }
            
            match self.send_cmd(Command::AppCmd(0)) {
                Some(_) => trace!("AppCmd succeeded"),
                None => {
                    warn!("AppCmd failed on attempt {}", attempt);
                    continue;
                }
            }
            
            match self.send_cmd(Command::SdSendOpCond(0x41FF_8000)) {
                Some(resp) => {
                    let ocr = resp[0];
                    if ocr & 0x8000_0000 != 0 {
                        warn!("SD card is ready after {} attempts", attempt);
                        card_initialized = true;
                        if ocr & 0x4000_0000 != 0 {
                            debug!("SD card supports high capacity");
                        } else {
                            debug!("SD card is standard capacity");
                        }
                        break;
                    } else {
                        trace!("SD card not ready yet, attempt {}, ocr: {ocr:x}", attempt);
                    }
                }
                None => {
                    warn!("SdSendOpCond failed on attempt {}", attempt);
                }
            }
            
            core::hint::spin_loop();
        }
        
        if !card_initialized {
            warn!("Card initialization failed - continuing anyway");
            return;  // Cannot continue without card
        }

        warn!("Sending AllSendCid command...");
        match self.send_cmd(Command::AllSendCid) {
            Some(resp) => {
                let cid = unsafe { core::mem::transmute::<[u32; 4], Cid>(resp) };
                warn!("cid: {cid:?}");
            }
            None => {
                warn!("AllSendCid failed - cannot determine card ID");
                return;
            }
        }

        warn!("Sending SendRelativeAddr command...");
        let rca = match self.send_cmd(Command::SendRelativeAddr) {
            Some(resp) => {
                let rca = (resp[0] >> 16) & 0xffff;
                debug!("rca: {rca:#x}");
                rca
            }
            None => {
                warn!("SendRelativeAddr failed - cannot get card address");
                return;
            }
        };

        warn!("Sending SendCsd command...");
        match self.send_cmd(Command::SendCsd(rca << 16)) {
            Some(resp) => {
                let csd = unsafe { core::mem::transmute::<[u32; 4], CsdV2>(resp) };
                debug!("csd: {csd:?}");
                self.num_blocks = csd.num_blocks();
                warn!("SD card capacity: {:#x} blocks", self.num_blocks);
            }
            None => {
                warn!("SendCsd failed - cannot determine card capacity");
                self.num_blocks = 0;
            }
        }

        warn!("Sending SelectCard command...");
        match self.send_cmd(Command::SelectCard(rca << 16)) {
            Some(_) => warn!("SelectCard succeeded"),
            None => warn!("SelectCard failed"),
        }

        warn!("Sending AppCmd command...");
        match self.send_cmd(Command::AppCmd(rca << 16)) {
            Some(_) => warn!("AppCmd succeeded"),
            None => warn!("AppCmd failed"),
        }

        // Read SCR register of SD card to determine supported bus widths.
        // This is needed before we can set the bus width.
        self.set_transaction_size(8, 8);
        // Although the SCR register is only 8 bytes, we allocate a 512-byte buffer here.
        // This is because many controllers and drivers require the buffer to be block-aligned (e.g., 512 bytes),
        // and only the first 8 bytes will be filled with valid data. The rest is ignored.
        // This ensures compatibility with hardware and avoids DMA or alignment issues.
        let mut buf = [0u8; 512];
        warn!("Sending SendScr command...");
        match self.send_cmd(Command::SendScr(&mut buf)) {
            Some(_) => warn!("SendScr succeeded"),
            None => warn!("SendScr failed"),
        }

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

        // Enable IDMAC for DMA transfer.
        self.enable_idmac();

        trace!("ctrl: {:?}", self.regs.ctrl().read());
        let rintsts = self.regs.rintsts().read();
        trace!("rintsts: {rintsts:?}");
        self.regs.rintsts().write(rintsts); // clear interrupt status

        info!("SD/MMC driver initialized");
    }

    /// Reads a single block from the SD/MMC card.
    pub fn read_block(&mut self, block: u32, buf: &mut [u8; 512]) {
        self.set_transaction_size(512, 512);
        // self.send_cmd(Command::ReadSingleBlock(block, buf)).unwrap();
        self.send_cmd_idmac(Command::ReadSingleBlock(block, buf)).unwrap();
        trace!("fifo count: {}", self.fifo_cnt());
    }

    /// Writes a single block to the SD/MMC card.
    pub fn write_block(&mut self, block: u32, buf: &[u8; 512]) {
        self.set_transaction_size(512, 512);
        // self.send_cmd(Command::WriteSingleBlock(block, buf)).unwrap();
        self.send_cmd_idmac(Command::WriteSingleBlock(block, buf)).unwrap();
        trace!("fifo count: {}", self.fifo_cnt());
    }

    /// Returns the number of blocks.
    pub fn num_blocks(&self) -> u64 {
        self.num_blocks
    }

    /// Enables the Internal DMA (IDMAC) for DMA transfers.
    pub fn enable_idmac(&mut self) {
        info!("Enabling IDMAC for DMA transfer");

        // reset part for reference
        //  self.regs.bmod().update(|r| r.with_de(false).with_swr(true));
        // self.regs
        //    .ctrl()
        //    .update(|r| r.with_dma_reset(true).with_use_internal_dmac(false));

        // Set the BMOD register to enable the internal DMA controller (IDMAC).
        // BMOD's PBL value is read-only value and is the mirror of MSIZE of FIFOTH register.
        // And the DSL value is applicable only for dual buffer structure.
        self.regs.bmod().update(|r| r.with_de(true).with_dsl(0).with_fb(true));
        
        // TODO: Enable interrupts.

        // Set the CTRL register to enable the use of the internal DMA controller (IDMAC).
        self.regs.ctrl().update(|r| r.with_use_internal_dmac(true));

        self.dma_enabled = true;

        info!("IDMAC enabled for DMA transfer");
    }

    /// The size of a block in bytes.
    pub const BLOCK_SIZE: usize = 512;
}

unsafe impl Send for SdMmc {}
unsafe impl Sync for SdMmc {}
