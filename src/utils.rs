use core::fmt;

use bitfield_struct::bitfield;

/// Card Identification
///
/// Reference: https://www.cameramemoryspeed.com/sd-memory-card-faq/reading-sd-card-cid-serial-psn-internal-numbers/
#[bitfield(u128, order = Msb, debug = false)]
pub struct Cid {
    /// Manufacturer ID
    pub mid: u8,
    /// OEM/Application ID
    pub oid: u16,
    /// Product name
    #[bits(40)]
    pub pnm: u64,
    /// Product Revision
    #[bits(8)]
    pub prv: ProductRevision,
    /// Product Serial Number
    pub psn: u32,
    /// Manufacturing Date
    #[bits(16)]
    pub mdt: ManufacturingDate,
    /// CRC7 checksum
    #[bits(7)]
    pub crc: u8,
    __: bool,
}

#[bitfield(u8, order = Msb, debug = false)]
pub struct ProductRevision {
    #[bits(4)]
    pub hwrev: u8,
    #[bits(4)]
    pub fwrev: u8,
}

impl fmt::Debug for ProductRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.hwrev(), self.fwrev())
    }
}

#[bitfield(u16, order = Msb, debug = false)]
pub struct ManufacturingDate {
    #[bits(4)]
    __: u8,
    /// Manufacture Date Code - Year
    pub year: u8,
    /// Manufacture Date Code - Month
    #[bits(4)]
    pub month: u8,
}

impl fmt::Debug for ManufacturingDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}/{}", self.month(), self.year() as u32 + 2000)
    }
}

impl fmt::Debug for Cid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cid")
            .field("mid", &self.mid())
            .field(
                "oid",
                &str::from_utf8(&self.oid().to_be_bytes()).unwrap_or("Invalid OEM ID"),
            )
            .field(
                "pnm",
                &str::from_utf8(&self.pnm().to_be_bytes()[3..8]).unwrap_or("Invalid Product Name"),
            )
            .field("prv", &self.prv())
            .field("psn", &self.psn())
            .field("mdt", &self.mdt())
            .field("crc", &self.crc())
            .finish()
    }
}

/// Card Specific Data, version 2
#[bitfield(u128, order = Msb)]
pub struct CsdV2 {
    /// CSD structure
    #[bits(2)]
    pub csd_structure: u8,
    #[bits(6)]
    __: u8,
    /// Data read access time 1
    pub taac: u8,
    /// Data write access time 1
    pub nsac: u8,
    /// Max data transfer rate
    pub tran_speed: u8,
    /// Card command class
    #[bits(12)]
    pub ccc: u16,
    /// Max read block length
    #[bits(4)]
    pub read_bl_len: u8,
    /// Partial blocks for read allowed
    pub read_blk_partial: bool,
    /// Write block misalignment
    pub write_blk_misaligned: bool,
    /// Read block misalignment
    pub read_blk_misaligned: bool,
    /// DSR implemented
    pub dsr_imp: bool,
    #[bits(6)]
    __: u8,
    /// Device size
    #[bits(22)]
    pub c_size: u32,
    __: bool,
    /// Erase single block enabled
    pub erase_blk_en: bool,
    /// Erase sector size
    #[bits(7)]
    pub sector_size: u8,
    /// Write protect group size
    #[bits(7)]
    pub wp_grp_size: u8,
    /// Write protect group enable
    pub wp_grp_enable: bool,
    #[bits(2)]
    __: u8,
    /// Write speed factor
    #[bits(3)]
    pub r2w_factor: u8,
    /// Max write block length
    #[bits(4)]
    pub write_bl_len: u8,
    /// Partial blocks for write allowed
    pub write_blk_partial: bool,
    #[bits(5)]
    __: u8,
    /// File format group
    pub file_format_grp: bool,
    /// Copy flag
    pub copy: bool,
    /// Permanent write protection
    pub perm_write_protect: bool,
    /// Temporary write protection
    pub tmp_write_protect: bool,
    /// File format
    #[bits(2)]
    pub file_format: u8,
    #[bits(2)]
    __: u8,
    /// CRC checksum
    #[bits(7)]
    pub crc: u8,
    __: bool,
}

impl CsdV2 {
    /// Returns the number of blocks.
    pub fn num_blocks(&self) -> u64 {
        (self.c_size() as u64 + 1) * 1024
    }
}

/// Card Specific Data Version 1 (Standard Capacity)
#[bitfield(u128, order = Msb)]
pub struct CsdV1 {
    /// CSD structure
    #[bits(2)]
    pub csd_structure: u8,
    #[bits(6)]
    __: u8,
    /// Data read access time 1
    pub taac: u8,
    /// Data write access time 1
    pub nsac: u8,
    /// Max data transfer rate
    pub tran_speed: u8,
    /// Card command class
    #[bits(12)]
    pub ccc: u16,
    /// Max read block length
    #[bits(4)]
    pub read_bl_len: u8,
    /// Partial blocks for read allowed
    pub read_blk_partial: bool,
    /// Write block misalignment
    pub write_blk_misaligned: bool,
    /// Read block misalignment
    pub read_blk_misaligned: bool,
    /// DSR implemented
    pub dsr_imp: bool,
    #[bits(2)]
    __: u8,
    /// Device size
    #[bits(12)]
    pub c_size: u16,
    /// Max. read current @ VDD min
    #[bits(3)]
    pub vdd_r_curr_min: u8,
    /// Max. read current @ VDD max
    #[bits(3)]
    pub vdd_r_curr_max: u8,
    /// Max. write current @ VDD min
    #[bits(3)]
    pub vdd_w_curr_min: u8,
    /// Max. write current @ VDD max
    #[bits(3)]
    pub vdd_w_curr_max: u8,
    /// Device size multiplier
    #[bits(3)]
    pub c_size_mult: u8,
    /// Erase single block enabled
    pub erase_blk_en: bool,
    /// Erase sector size
    #[bits(7)]
    pub sector_size: u8,
    /// Write protect group size
    #[bits(7)]
    pub wp_grp_size: u8,
    /// Write protect group enable
    pub wp_grp_enable: bool,
    #[bits(2)]
    __: u8,
    /// Write speed factor
    #[bits(3)]
    pub r2w_factor: u8,
    /// Max write block length
    #[bits(4)]
    pub write_bl_len: u8,
    /// Partial blocks for write allowed
    pub write_blk_partial: bool,
    #[bits(5)]
    __: u8,
    /// File format group
    pub file_format_grp: bool,
    /// Copy flag
    pub copy: bool,
    /// Permanent write protection
    pub perm_write_protect: bool,
    /// Temporary write protection
    pub tmp_write_protect: bool,
    /// File format
    #[bits(2)]
    pub file_format: u8,
    #[bits(2)]
    __: u8,
    /// CRC checksum
    #[bits(7)]
    pub crc: u8,
    __: bool,
}

impl CsdV1 {
    /// Returns the capacity of the card in bytes.
    ///
    /// The formula for CSD version 1.0 is:
    /// Capacity = (C_SIZE + 1) * 2^(C_SIZE_MULT + 2) * 2^(READ_BL_LEN)
    pub fn capacity(&self) -> u64 {
        let c_size = self.c_size() as u64;
        let c_size_mult = self.c_size_mult() as u64;
        let read_bl_len = self.read_bl_len() as u64;

        let mult = 1 << (c_size_mult + 2);
        let block_len = 1 << read_bl_len;

        (c_size + 1) * mult * block_len
    }

    /// Returns the number of blocks.
    /// Assumes a block size of 512 bytes for compatibility with standard block drivers.
    pub fn num_blocks(&self) -> u64 {
        self.capacity() / 512
    }
}

/// SD Card Configuration Register
#[bitfield(u64, order = Msb)]
pub struct Scr {
    #[bits(4)]
    pub scr_structure: u8,
    #[bits(4)]
    pub sd_spec: u8,
    pub data_stat_after_erase: bool,
    #[bits(3)]
    pub sd_security: u8,
    #[bits(4)]
    pub sd_bus_widths: u8,
    pub sd_spec3: bool,
    #[bits(4)]
    pub ex_security: u8,
    pub sd_spec4: bool,
    #[bits(6)]
    __: u8,
    #[bits(4)]
    pub cmd_support: u8,
    #[bits(32)]
    __: u32,
}

/// SD Status (512 bits / 64 bytes)
///
/// NOTE: Not a bitfield struct because the size exceeds u128.
/// Wraps a 64-byte array and provides accessors for common fields.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SdStatus(pub [u8; 64]);

impl SdStatus {
    /// Data bus width (bits 511:510)
    pub fn dat_bus_width(&self) -> u8 {
        (self.0[0] >> 6) & 0b11
    }

    /// Secured mode (bit 509)
    pub fn secured_mode(&self) -> bool {
        (self.0[0] >> 5) & 1 != 0
    }

    /// SD Card Type (bits 495:480)
    pub fn sd_card_type(&self) -> u16 {
        let b2 = self.0[2]; // 495:488
        let b3 = self.0[3]; // 487:480
        ((b2 as u16) << 8) | (b3 as u16)
    }

    /// Raw size of the protected area (bits 479:448).
    pub fn size_of_protected_area_raw(&self) -> u32 {
        u32::from_be_bytes([self.0[4], self.0[5], self.0[6], self.0[7]])
    }

    /// Size of the protected area reported in bytes.
    ///
    /// For SDHC/SDXC cards the raw value is already expressed in bytes so
    /// `scaling_unit` should be `None`. Standard-capacity SDSC cards express the
    /// field in units of `MULT * BLOCK_LEN` (see SD Physical Layer spec Table 4-43),
    /// therefore callers should pass `Some(mult_block_len_bytes)` where
    /// `mult_block_len_bytes` equals that SDSC multiplier in bytes.
    pub fn size_of_protected_area_bytes(&self, scaling_unit: Option<u64>) -> u64 {
        let raw = self.size_of_protected_area_raw() as u64;
        scaling_unit.map_or(raw, |unit| raw.saturating_mul(unit))
    }

    /// Speed Class (bits 447:440)
    pub fn speed_class(&self) -> u8 {
        self.0[8]
    }

    /// Performance Move (bits 439:432)
    pub fn performance_move(&self) -> u8 {
        self.0[9]
    }

    /// AU Size (bits 431:428)
    pub fn au_size(&self) -> u8 {
        self.0[10] >> 4
    }

    /// Erase Size (bits 423:408)
    pub fn erase_size(&self) -> u16 {
        let b11 = self.0[11]; // 423:416 -> bits 7:0 of byte 11
        let b12 = self.0[12]; // 415:408 -> bits 7:0 of byte 12
        ((b11 as u16) << 8) | (b12 as u16)
    }

    /// Erase Timeout (bits 407:402)
    pub fn erase_timeout(&self) -> u8 {
        self.0[13] >> 2
    }

    /// Erase Offset (bits 401:400)
    pub fn erase_offset(&self) -> u8 {
        self.0[13] & 0b11
    }

    /// UHS Speed Grade (bits 399:396)
    pub fn uhs_speed_grade(&self) -> u8 {
        self.0[14] >> 4
    }

    /// UHS AU Size (bits 395:392)
    pub fn uhs_au_size(&self) -> u8 {
        self.0[14] & 0x0F
    }
    
    /// Video Speed Class (bits 391:388)
    pub fn video_speed_class(&self) -> u8 {
        self.0[15] >> 4
    }
}

impl fmt::Debug for SdStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SdStatus")
            .field("dat_bus_width", &self.dat_bus_width())
            .field("secured_mode", &self.secured_mode())
            .field("sd_card_type", &self.sd_card_type())
            .field(
                "size_of_protected_area_raw",
                &self.size_of_protected_area_raw(),
            )
            .field("speed_class", &self.speed_class())
            .field("performance_move", &self.performance_move())
            .field("au_size", &self.au_size())
            .field("erase_size", &self.erase_size())
            .field("erase_timeout", &self.erase_timeout())
            .field("erase_offset", &self.erase_offset())
            .field("uhs_speed_grade", &self.uhs_speed_grade())
            .field("uhs_au_size", &self.uhs_au_size())
            .field("video_speed_class", &self.video_speed_class())
            .finish()
    }
}
