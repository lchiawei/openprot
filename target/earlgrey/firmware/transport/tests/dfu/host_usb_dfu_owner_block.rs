// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fs;
use std::time::Duration;

use earlgrey_testutil::{
    get_dfu_transfer_size, parse_owner_block, print_uart, sequence_dfu_download,
    sequence_dfu_upload, DfuClient,
};
use opentitanlib::app::TransportWrapper;
use opentitanlib::ownership::OwnerBlock;
use opentitanlib::test_utils::init::InitializeTest;
use opentitanlib::uart::console::UartConsole;
use usb::UsbOpts;

#[derive(Parser, Debug)]
struct CmdArgs {
    #[command(flatten)]
    init: InitializeTest,

    #[command(flatten)]
    usb: UsbOpts,

    #[arg(
        long,
        default_value = "target/earlgrey/firmware/transport/tests/dfu/transport_transfer.app_prod_0.signed.bin"
    )]
    firmware: String,

    #[arg(
        long,
        default_value = "target/earlgrey/signing/keys/dummy/dummy_owner_bin.bin"
    )]
    golden_owner_block: String,
}

fn run_dfu_owner_block_test(
    transport: &TransportWrapper,
    usb: &UsbOpts,
    firmware_path: &str,
    golden_owner_block_path: &str,
) -> Result<()> {
    let uart = transport.uart("console")?;

    log::info!("Resetting target...");
    transport.reset(opentitanlib::app::UartRx::Clear)?;

    log::info!("waiting for Maize Welcome on console...");
    let _ = UartConsole::wait_for(
        &*uart,
        r"Welcome to Maize on Earlgrey Transport Firmware!",
        Duration::from_secs(10),
    )?;

    usb.apply_strappings(transport, true)?;
    if usb.vbus_control_available() {
        usb.enable_vbus(transport, true)?;
    }
    if usb.vbus_sense_available() {
        if !usb.vbus_present(transport)? {
            bail!("OT USB does not appear to be connected to a host (VBUS not detected)");
        }
    }

    let usb_vid = usb.vid;
    let usb_pid = usb.pid;

    log::info!(
        "Reading Transport firmware payload from '{}'...",
        firmware_path
    );
    let firmware_data = fs::read(firmware_path)
        .with_context(|| format!("Failed to read firmware from {}", firmware_path))?;

    {
        log::info!(
            "waiting for DFU device (VID={:04x}, PID={:04x}) for Provisioning...",
            usb_vid,
            usb_pid
        );
        let device = transport
            .usb()?
            .device_by_id_with_timeout(usb_vid, usb_pid, None, Duration::from_secs(10))
            .context("DFU device not found for provisioning phase")?;

        log::info!("Claiming DFU interface for Provisioning...");
        let interface_num = 2;
        device.claim_interface(interface_num)?;

        let transfer_size = get_dfu_transfer_size(&*device, interface_num)?;
        log::info!("DFU Transfer Size (Block Size): {} bytes", transfer_size);

        let dfu = DfuClient::new(&*device, interface_num);

        log::info!("Sequencing DFU Download on Alt 0 to provision OWNER_PAGE_1 and update slot...");
        sequence_dfu_download(&dfu, &*uart, &firmware_data, transfer_size, true)?;

        let _ = device.release_interface(interface_num);
    }

    log::info!("Waiting for second 'Maize Welcome' on console after manifestation reboot...");
    let _ = UartConsole::wait_for(
        &*uart,
        r"Welcome to Maize on Earlgrey Transport Firmware!",
        Duration::from_secs(20),
    )
    .context("Failed to detect Maize Welcome after provisioning reboot!")?;
    log::info!("✅ Transport DFU Server rebooted successfully after provisioning!");

    // Give the USB stack a brief moment to finish attaching and enumerating after the UART banner
    std::thread::sleep(Duration::from_millis(500));

    log::info!(
        "waiting for DFU device (VID={:04x}, PID={:04x}) for Upload phase...",
        usb_vid,
        usb_pid
    );
    let device = transport
        .usb()?
        .device_by_id_with_timeout(usb_vid, usb_pid, None, Duration::from_secs(10))
        .context("DFU device not found for upload phase")?;

    log::info!("Claiming DFU interface for Upload phase...");
    let interface_num = 2;
    device.claim_interface(interface_num)?;

    let transfer_size = get_dfu_transfer_size(&*device, interface_num)?;
    log::info!("DFU Transfer Size (Block Size): {} bytes", transfer_size);

    log::info!("Setting USB DFU Alt setting to 4 (Owner Block)...");
    device.set_alternate_setting(interface_num, 4)?;

    let dfu = DfuClient::new(&*device, interface_num);

    log::info!("Reading owner block from Alt 4 via sequence_dfu_upload...");
    let buf = sequence_dfu_upload(&dfu, OwnerBlock::SIZE, transfer_size)?;
    let _ = device.release_interface(interface_num);

    log::info!("Parsing retrieved Owner Block from DFU upload...");
    let mut result_owner_block =
        parse_owner_block(&buf).context("Failed to parse read OwnerBlock")?;

    log::info!(
        "Reading and parsing Golden Owner Block from '{}'...",
        golden_owner_block_path
    );
    let golden_data = fs::read(golden_owner_block_path).with_context(|| {
        format!(
            "Failed to read Golden Owner Block from {}",
            golden_owner_block_path
        )
    })?;
    if golden_data.len() != OwnerBlock::SIZE {
        bail!(
            "Expected Golden Owner Block to be {} bytes, got {}",
            OwnerBlock::SIZE,
            golden_data.len()
        );
    }
    let golden_owner_block =
        parse_owner_block(&golden_data).context("Failed to parse golden OwnerBlock")?;

    log::info!("================ READ OWNER BLOCK FIELDS ================");
    log::info!("{:#?}", result_owner_block);
    log::info!("=========================================================");
    let zero_seal = vec![0u8; 32];

    if result_owner_block.seal == zero_seal {
        log::error!("❌ FAILURE: Readout OwnerBlock seal is ALL ZEROES! Expected ROM_EXT to have sealed OWNER_PAGE_1!");
        bail!("Readout OwnerBlock seal is all zeroes!");
    }

    // Set the seal to all zeroes before comparison because we do not have seal
    // value in golden dummy file.
    result_owner_block.seal = zero_seal;
    log::info!("Comparing Readout OwnerBlock with local Golden OwnerBlock directly using '=='...");
    if result_owner_block == golden_owner_block {
        log::info!("✅ SUCCESS: Readout OwnerBlock MATCHES Golden OwnerBlock perfectly!");
    } else {
        log::info!("⚠️ NOTE: Read OwnerBlock from Flash has a different Seal, Signature, or static configuration than the local Golden dummy OwnerBlock!");
        log::info!("================ READ OWNER BLOCK FIELDS ================");
        log::info!("{:#?}", result_owner_block);
        log::info!("================ GOLDEN OWNER BLOCK FIELDS ================");
        log::info!("{:#?}", golden_owner_block);
        log::info!("===========================================================");
        log::error!("❌ FAILURE: Read OwnerBlock from Flash has a different Signature, or static configuration than the local Golden dummy OwnerBlock!");
        bail!("Read OwnerBlock is different from Golden dummy OwnerBlock!")
    }

    print_uart(&*uart);
    log::info!("Test Execution Finished Successfully!");
    Ok(())
}

fn main() -> Result<()> {
    let args = CmdArgs::parse();
    args.init.init_logging();

    let transport = args.init.init_target()?;
    run_dfu_owner_block_test(
        &transport,
        &args.usb,
        &args.firmware,
        &args.golden_owner_block,
    )?;
    Ok(())
}
