//! `victron-cli decode-fixture` — decode a captured notification fixture.

use std::path::PathBuf;

use clap::Args;

use crate::CliError;

#[derive(Debug, Args)]
pub struct DecodeFixture {
    /// Path to a sanitized notification fixture (Data/LastData bytes).
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// VE.Smart instance the fixture was captured for.
    #[arg(long, default_value_t = 3)]
    pub instance: u16,

    /// Print decoded raw VREG values instead of the summary.
    #[arg(long)]
    pub verbose: bool,
}

impl DecodeFixture {
    pub fn run(&self) -> Result<(), CliError> {
        let _ = (&self.path, self.instance, self.verbose);
        Err(crate::not_wired(
            "requires victron-protocol (CBOR chunk reassembly + opcode/VREG \
             decoding); protocol lane wiring pending. Capture fixtures with \
             scripts/read-victron-live-values.py.",
        ))
    }
}
