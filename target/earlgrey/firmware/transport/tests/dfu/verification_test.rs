// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

// OpenTitan host libraries
use cryptotest_commands as _;
use opentitanlib as _;
use ot_hal as _;
use ot_transport_chipwhisperer as _;
use ot_transport_hyperdebug as _;
use ot_transport_verilator as _;
use sphincsplus as _;
use ujson_lib as _;

// OpenProt host libraries
use earlgrey_testutil as _;
use openprot as _;
use openprot_mctp_api as _;
use openprot_mctp_server as _;
use util_types as _;

fn main() -> Result<()> {
    println!("SUCCESS: Successfully compiled, linked, and executed binary combining EXHAUSTIVE list of OpenTitan and OpenProt host libraries!");
    println!("- OpenTitan host libraries used: opentitanlib, ot_hal, sphincsplus, cryptotest_commands, ujson_lib, ot_transport_chipwhisperer, ot_transport_hyperdebug, ot_transport_verilator");
    println!("- OpenProt host libraries used: openprot, openprot_mctp_api, openprot_mctp_server, earlgrey_testutil, util_types");
    Ok(())
}
