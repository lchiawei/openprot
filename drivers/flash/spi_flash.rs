// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI Flash driver.
//!
//! Implements `hal::blocking::flash::FlashDriver` for generic SPI NOR Flash chips
//! using `embedded-hal` SPI interfaces and SFDP discovery.

#![no_std]
#![allow(clippy::unnecessary_cast)]

use core::num::NonZero;
use hal_flash_driver::{FlashAddress, FlashDriver};
use util_error as error;
use util_error::ErrorCode;
use util_io::RandomRead;
use util_types::PowerOf2Usize;

const OP_WRITE_EN: u8 = 0x06;
const OP_READ_STATUS: u8 = 0x05;
const STATUS_WIP_MASK: u8 = 0x01;

#[derive(Clone, Copy)]
struct SfCmd {
    opcode: u8,
    addr_bytes: u8,
    dummy_bytes: u8,
}

/// SPI Flash driver.
pub struct SpiFlash<S: embedded_hal::spi::SpiDevice> {
    spi: S,
    size_bytes: usize,
    erasable_sizes_bitmap: u32,
    read_cmd: SfCmd,
    program_cmd: SfCmd,
    erase_4k_opcode: u8,
    erase_64k_opcode: u8,
    addr_mode_4b: bool,
    initialized: bool,
}

impl<S: embedded_hal::spi::SpiDevice> SpiFlash<S> {
    /// Creates a new, uninitialized `SpiFlash` driver.
    ///
    /// Must call `init()` before performing any flash operations.
    pub fn new(spi: S) -> Self {
        Self {
            spi,
            size_bytes: 0,
            erasable_sizes_bitmap: 0,
            read_cmd: SfCmd {
                opcode: 0,
                addr_bytes: 0,
                dummy_bytes: 0,
            },
            program_cmd: SfCmd {
                opcode: 0,
                addr_bytes: 0,
                dummy_bytes: 0,
            },
            erase_4k_opcode: 0,
            erase_64k_opcode: 0,
            addr_mode_4b: false,
            initialized: false,
        }
    }

    /// Initializes the driver by reading and parsing SFDP from the chip.
    pub fn init(&mut self) -> Result<(), ErrorCode> {
        let sfdp_reader = SfdpPhysicalReader::new(&mut self.spi);
        let mut sfdp = util_sfdp::SfdpReader::new(sfdp_reader)
            .map_err(|_| error::FLASH_GENERIC_SFDP_INVALID_SIGNATURE)?;

        let table = sfdp
            .basic_flash_parameters()
            .map_err(|_| error::FLASH_GENERIC_SFDP_PARAMETERS_TOO_SHORT)?;

        let size = table
            .table_jesd216()
            .memory_density
            .byte_len()
            .map_err(|_| error::FLASH_GENERIC_SFDP_INVALID_MEMORY_DENSITY)?;

        self.size_bytes = size as usize;
        self.erasable_sizes_bitmap = (1 << 12) | (1 << 16); // Support 4KB (bit 12) and 64KB (bit 16)

        // Verify page size if JESD216A+ is supported
        if let Some(jesd216a) = table.table_jesd216a() {
            let page_size = jesd216a.word11.page_size().value();
            if page_size != 256 {
                pw_log::error!(
                    "Unsupported flash page size: {} (expected 256)",
                    page_size as u32
                );
                return Err(error::FLASH_GENERIC_INVALID_PAGE_SIZE);
            }
        }

        // Configure opcodes based on capacity (explicitly enter 4-byte mode if > 16MB)
        if size <= 16 * 1024 * 1024 {
            self.read_cmd = SfCmd {
                opcode: 0x03, // Read Data
                addr_bytes: 3,
                dummy_bytes: 0,
            };
            self.program_cmd = SfCmd {
                opcode: 0x02, // Page Program
                addr_bytes: 3,
                dummy_bytes: 0,
            };
            self.erase_4k_opcode = 0x20; // Sector Erase 4KB
            self.erase_64k_opcode = 0xD8; // Block Erase 64KB
            self.addr_mode_4b = false;
        } else {
            // Explicitly enter 4-byte address mode
            self.enter_4byte_mode()?;
            
            // Standard read/write opcodes now expect 4-byte addresses
            self.read_cmd = SfCmd {
                opcode: 0x03, // Read Data (4B address)
                addr_bytes: 4,
                dummy_bytes: 0,
            };
            self.program_cmd = SfCmd {
                opcode: 0x02, // Page Program (4B address)
                addr_bytes: 4,
                dummy_bytes: 0,
            };
            self.erase_4k_opcode = 0x20; // Sector Erase 4KB (4B address)
            self.erase_64k_opcode = 0xD8; // Block Erase 64KB (4B address)
            self.addr_mode_4b = true;
        }

        self.initialized = true;
        pw_log::info!(
            "SPI Flash initialized: size={} bytes, 4B_mode={}",
            size as u32,
            self.addr_mode_4b as u32
        );
        Ok(())
    }

    fn enter_4byte_mode(&mut self) -> Result<(), ErrorCode> {
        self.write_enable()?;
        self.spi
            .write(&[0xB7]) // EN4B: Enter 4-Byte Address Mode
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        Ok(())
    }

    fn write_enable(&mut self) -> Result<(), ErrorCode> {
        self.spi
            .write(&[OP_WRITE_EN])
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        Ok(())
    }

    pub fn read_status(&mut self) -> Result<u8, ErrorCode> {
        let tx = [OP_READ_STATUS];
        let mut rx = [0u8];
        let mut ops = [
            embedded_hal::spi::Operation::Write(&tx),
            embedded_hal::spi::Operation::Read(&mut rx),
        ];
        self.spi
            .transaction(&mut ops)
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        let [val] = rx;
        Ok(val)
    }
}

impl<S: embedded_hal::spi::SpiDevice> FlashDriver for SpiFlash<S> {
    type Error = ErrorCode;

    // Standard SPI Flash parameters
    const PAGE_SIZE: usize = 256;
    const PROGRAM_WINDOW_SIZE: usize = 256;
    const MAX_READ_SIZE: usize = 256;
    const READ_ALIGNMENT: usize = 1;
    const PROGRAM_ALIGNMENT: usize = 1;

    fn size(&self) -> NonZero<usize> {
        // SAFETY: 1 is non-zero.
        let default_size = unsafe { NonZero::new_unchecked(1) };
        NonZero::new(self.size_bytes).unwrap_or(default_size)
    }

    fn erasable_sizes_bitmap(&mut self) -> Result<u32, Self::Error> {
        if !self.initialized {
            return Err(error::FLASH_GENERIC_NOT_INITIALIZED);
        }
        Ok(self.erasable_sizes_bitmap)
    }

    fn read(&mut self, start_addr: FlashAddress, buf: &mut [u8]) -> Result<(), Self::Error> {
        if !self.initialized {
            return Err(error::FLASH_GENERIC_NOT_INITIALIZED);
        }
        if buf.is_empty() {
            return Ok(());
        }

        let offset = start_addr.offset() as usize;
        let end_offset = offset
            .checked_add(buf.len())
            .ok_or(error::FLASH_GENERIC_ADDR_OUT_OF_BOUNDS)?;
        if end_offset > self.size_bytes {
            return Err(error::FLASH_GENERIC_ADDR_OUT_OF_BOUNDS);
        }

        let mut prefix = [0u8; 9];
        let mut len = 0;
        *prefix
            .get_mut(len)
            .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = self.read_cmd.opcode;
        len += 1;

        let addr = start_addr.offset();
        if self.read_cmd.addr_bytes == 3 {
            *prefix
                .get_mut(len)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = (addr >> 16) as u8;
            *prefix
                .get_mut(len + 1)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = (addr >> 8) as u8;
            *prefix
                .get_mut(len + 2)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = addr as u8;
            len += 3;
        } else {
            *prefix
                .get_mut(len)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = (addr >> 24) as u8;
            *prefix
                .get_mut(len + 1)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = (addr >> 16) as u8;
            *prefix
                .get_mut(len + 2)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = (addr >> 8) as u8;
            *prefix
                .get_mut(len + 3)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = addr as u8;
            len += 4;
        }

        for _ in 0..self.read_cmd.dummy_bytes {
            *prefix
                .get_mut(len)
                .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)? = 0;
            len += 1;
        }

        let mut ops = [
            embedded_hal::spi::Operation::Write(
                prefix
                    .get(..len)
                    .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)?,
            ),
            embedded_hal::spi::Operation::Read(buf),
        ];

        self.spi
            .transaction(&mut ops)
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        Ok(())
    }

    fn start_erase(
        &mut self,
        start_addr: FlashAddress,
        size: PowerOf2Usize,
    ) -> Result<(), Self::Error> {
        if !self.initialized {
            return Err(error::FLASH_GENERIC_NOT_INITIALIZED);
        }

        let offset = start_addr.offset() as usize;
        if offset % size.get() != 0 {
            return Err(error::FLASH_GENERIC_ERASE_INVALID_ADDR);
        }
        let end_offset = offset
            .checked_add(size.get())
            .ok_or(error::FLASH_GENERIC_ADDR_OUT_OF_BOUNDS)?;
        if end_offset > self.size_bytes {
            return Err(error::FLASH_GENERIC_ADDR_OUT_OF_BOUNDS);
        }

        let size_log2 = size.get().trailing_zeros() as u8;
        let opcode = match size_log2 {
            12 => self.erase_4k_opcode,  // 4KB Erase
            16 => self.erase_64k_opcode, // 64KB Erase
            _ => return Err(error::FLASH_GENERIC_ERASE_INVALID_SIZE),
        };

        self.write_enable()?;

        let addr = start_addr.offset();
        let (prefix, len) = if self.addr_mode_4b {
            (
                [
                    opcode,
                    (addr >> 24) as u8,
                    (addr >> 16) as u8,
                    (addr >> 8) as u8,
                    addr as u8,
                ],
                5,
            )
        } else {
            (
                [opcode, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8, 0],
                4,
            )
        };

        self.spi
            .write(
                prefix
                    .get(..len)
                    .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)?,
            )
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        Ok(())
    }

    fn start_program(
        &mut self,
        start_address: FlashAddress,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        if !self.initialized {
            return Err(error::FLASH_GENERIC_NOT_INITIALIZED);
        }
        if data.is_empty() {
            return Ok(());
        }

        let offset = start_address.offset() as usize;
        let end_offset = offset
            .checked_add(data.len())
            .ok_or(error::FLASH_GENERIC_ADDR_OUT_OF_BOUNDS)?;
        if end_offset > self.size_bytes {
            return Err(error::FLASH_GENERIC_ADDR_OUT_OF_BOUNDS);
        }

        self.write_enable()?;

        let addr = start_address.offset();
        let (prefix, len) = if self.program_cmd.addr_bytes == 3 {
            (
                [
                    self.program_cmd.opcode,
                    (addr >> 16) as u8,
                    (addr >> 8) as u8,
                    addr as u8,
                    0,
                ],
                4,
            )
        } else {
            (
                [
                    self.program_cmd.opcode,
                    (addr >> 24) as u8,
                    (addr >> 16) as u8,
                    (addr >> 8) as u8,
                    addr as u8,
                ],
                5,
            )
        };

        let mut ops = [
            embedded_hal::spi::Operation::Write(
                prefix
                    .get(..len)
                    .ok_or(error::FLASH_GENERIC_BAD_ALIGNMENT)?,
            ),
            embedded_hal::spi::Operation::Write(data),
        ];

        self.spi
            .transaction(&mut ops)
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        Ok(())
    }

    fn is_busy(&mut self) -> bool {
        if !self.initialized {
            return false;
        }
        match self.read_status() {
            Ok(status) => (status & STATUS_WIP_MASK) != 0,
            Err(_) => true,
        }
    }

    fn complete_op(&mut self) -> Result<(), Self::Error> {
        if !self.initialized {
            return Err(error::FLASH_GENERIC_NOT_INITIALIZED);
        }
        if self.is_busy() {
            return Err(error::FLASH_GENERIC_BUSY);
        }
        Ok(())
    }
}

/// Helper structure to read SFDP raw bytes from physical SPI flash.
struct SfdpPhysicalReader<'a, S: embedded_hal::spi::SpiDevice> {
    spi: &'a mut S,
}

impl<'a, S: embedded_hal::spi::SpiDevice> SfdpPhysicalReader<'a, S> {
    fn new(spi: &'a mut S) -> Self {
        Self { spi }
    }
}

impl<S: embedded_hal::spi::SpiDevice> RandomRead for SfdpPhysicalReader<'_, S> {
    type Error = ErrorCode;

    fn read(&mut self, start_addr: usize, dst: &mut [u8]) -> Result<(), Self::Error> {
        let prefix = [
            0x5A, // OP_SFDP_READ
            (start_addr >> 16) as u8,
            (start_addr >> 8) as u8,
            start_addr as u8,
            0, // Dummy byte
        ];

        let mut ops = [
            embedded_hal::spi::Operation::Write(&prefix),
            embedded_hal::spi::Operation::Read(dst),
        ];

        self.spi
            .transaction(&mut ops)
            .map_err(|_| error::FLASH_GENERIC_BUSY)?;
        Ok(())
    }

    fn size(&mut self) -> Result<usize, Self::Error> {
        // SFDP space size is not strictly defined but 1<<24 (16MB) is a safe maximum limit.
        Ok(1 << 24)
    }
}
