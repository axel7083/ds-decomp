use std::{
    fs::{self, File},
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
use clap::Args;
use clap_num::maybe_hex;
use ds_rom::rom::{self, Header, OverlayConfig};

use crate::analysis::functions::Function;

/// Disassembles overlays.
#[derive(Debug, Args)]
pub struct Overlay {
    /// Path to header.yaml.
    #[arg(short = 'H', long)]
    header_path: PathBuf,

    /// Path to armX_overlays.yaml.
    #[arg(short = 'l', long)]
    overlay_list_path: PathBuf,

    /// ID of overlay to disassemble.
    #[arg(short = 'i', long)]
    overlay_id: u32,

    /// Address to start disassembling from.
    #[arg(short = 's', long, value_parser=maybe_hex::<u32>)]
    start_address: Option<u32>,

    /// Number of functions to disassemble.
    #[arg(short = 'n', long)]
    num_functions: Option<usize>,
}

impl Overlay {
    pub fn run(&self) -> Result<()> {
        let header: Header = serde_yml::from_reader(File::open(&self.header_path)?)?;

        let overlay_configs: Vec<OverlayConfig> = serde_yml::from_reader(File::open(&self.overlay_list_path)?)?;
        let Some(overlay_config) = overlay_configs.into_iter().find(|c| c.info.id == self.overlay_id) else {
            bail!("Overlay ID {} not found in {}", self.overlay_id, self.overlay_list_path.display());
        };

        let data = fs::read(
            self.overlay_list_path.parent().context("overlay list path has no parent")?.join(overlay_config.file_name),
        )?;

        let overlay = rom::Overlay::new(data, header.version(), overlay_config.info);

        let functions = Function::iter_from_code(overlay.code(), overlay.base_address(), self.start_address);
        for function in functions.take(self.num_functions.unwrap_or(usize::MAX)) {
            println!("{function}");
        }

        Ok(())
    }
}
