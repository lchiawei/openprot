#![no_std]
#![no_main]

use spi_tpm_bridge_codegen::handle;
use pw_status::Result;
use userspace::time::Instant;
use ureg;
use userspace::{entry, syscall};
use registers::spi_device::*;

const TPM_WRITE_START_WORDS: usize = 0x180 / 4; 
const TPM_WRITE_LEN_WORDS: usize = 64 / 4;

fn init_spi_tpm(regs: &mut RegisterBlock<ureg::RealMmioMut<'_>>) {
    regs.tpm_cfg().write(|w| w.en(true).tpm_mode(false));
    pw_log::info!("🔌 Bridge: SPI TPM Hardware initialized!");
}

#[entry]
fn entry() -> Result<()> {
    pw_log::info!("🔌 Bridge: SPI TPM Bridge starting...");

    let mut spi_dev = unsafe { SpiDevice::new() };
    let mut regs = spi_dev.regs_mut();
    init_spi_tpm(&mut regs);

    let mut data_buffer = [0u8; 64]; 
    loop {
        let status = regs.tpm_status().read();
        if status.cmdaddr_notempty() {

            let cmd_addr = regs.tpm_cmd_addr().read();
            let address = cmd_addr.addr();
            
            let is_read = (cmd_addr.cmd() & 0x80) != 0;
            // Only process access to TPM_DATA_FIFO (0x24)
            if address == 0x024 {
                let xfer_size = ((cmd_addr.cmd() & 0x3F) + 1) as usize;

                if !is_read {
                    // 📥 Host is writing command ➡️ We collect data, and launch the transaction!
                    pw_log::info!("📥 Bridge: Host is writing {} bytes to SPI...", xfer_size);

                    // 1. Read data from Ingress SRAM
                    let write_fifo = regs.ingress_buffer()
                        .get_sub_array::<TPM_WRITE_LEN_WORDS>(TPM_WRITE_START_WORDS)
                        .unwrap();

                    for i in 0..TPM_WRITE_LEN_WORDS {
                        let offset = i * 4;
                        if offset < xfer_size {
                            let words = write_fifo.get(i).unwrap().read();
                            let bytes = words.to_le_bytes();
                            for (j, b) in bytes.iter().enumerate() {
                                if offset + j < xfer_size {
                                    data_buffer[offset + j] = *b;
                                }
                            }
                        }
                    }

                    // 2. 🔴 Crucial Maize standard call: launch channel_transact!
                    // Sends data_buffer, blocks until mock_tpm responds and writes the result into resp_buffer!
                    pw_log::info!("🔌 Bridge: Transacting via channel_transact...");
                    let mut resp_buffer = [0u8; 64];
                    let transact_result = syscall::channel_transact(
                        handle::IPC,
                        &data_buffer[..xfer_size],
                        &mut resp_buffer,
                        Instant::MAX, // 👈 Wait indefinitely until TPM finishes the response
                    );

                    match transact_result {
                        Ok(resp_len) => {
                            pw_log::info!("🔌 Bridge: Transact success! Received response (len={})", resp_len);
                            
                            // 3. Write the received Response back to tpm_read_fifo register!
                            for b in resp_buffer[..resp_len].iter() {
                                regs.tpm_read_fifo().write(|_| *b as u32);
                            }
                            pw_log::info!("✅ Bridge: Response written to SPI Read FIFO!");
                        }
                        Err(e) => {
                            pw_log::error!("❌ Bridge: channel_transact failed: {}", e as u32);
                        }
                    }
                } else {
                    // 📤 When Host is reading, the data has already been written into the register after a successful transact, no need to repeat
                    pw_log::info!("📤 Bridge: Host is reading response bytes from SPI FIFO...");
                }
            }

        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    pw_log::error!("FAIL: panic in spi_tpm_bridge!");
    let _ = syscall::debug_shutdown(Err(pw_status::Error::Unknown));
    loop {}
}
