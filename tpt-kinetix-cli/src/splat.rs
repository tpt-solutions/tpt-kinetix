//! `tpt-kinetix splat`: convert and inspect Gaussian splat files.
//!
//! Formats are recognised by their magic bytes, not their extensions:
//! PLY (`ply`), SPZ (gzip `1f 8b` or `NGSP`), `SPLT` (static 3D) and `SPLS`
//! (4D sequence).

use std::path::Path;

use anyhow::{bail, Context, Result};
use tpt_kinetix_splat::{
    read_ply, read_sequence, read_splt, read_spz, write_ply, write_splt, write_spz,
    CoordinateFrame, GaussianCloud, Quantization,
};

/// A splat file's format, from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ply,
    Spz,
    Splt,
    Spls,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Ply => "PLY (3D Gaussian splats)",
            Format::Spz => "SPZ (3D Gaussian splats)",
            Format::Splt => "SPLT (3D Gaussian splats, native codec)",
            Format::Spls => "SPLS (4D Gaussian splat sequence)",
        }
    }
}

/// Recognises a splat format from the first bytes of a file.
pub fn detect(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"ply\n") || bytes.starts_with(b"ply\r\n") {
        Some(Format::Ply)
    } else if bytes.starts_with(&[0x1f, 0x8b]) || bytes.starts_with(b"NGSP") {
        Some(Format::Spz)
    } else if bytes.starts_with(b"SPLT") {
        Some(Format::Splt)
    } else if bytes.starts_with(b"SPLS") {
        Some(Format::Spls)
    } else {
        None
    }
}

/// Reads a static 3D cloud from a PLY, SPZ or SPLT file.
fn load_cloud(path: &Path) -> Result<GaussianCloud> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    match detect(&bytes) {
        Some(Format::Ply) => Ok(read_ply(&bytes)?),
        Some(Format::Spz) => Ok(read_spz(&bytes)?),
        Some(Format::Splt) => Ok(read_splt(&bytes)?),
        Some(Format::Spls) => bail!("{} is a 4D sequence, not a static cloud", path.display()),
        None => bail!("{}: not a recognised splat file", path.display()),
    }
}

/// Converts a static splat file to another static format, chosen by the output
/// extension: `.splt` (native codec), `.spz` or `.ply`.
pub fn convert(input: &Path, output: &Path, quant: Quantization) -> Result<()> {
    let cloud = load_cloud(input)?;
    let bytes = match output.extension().and_then(|e| e.to_str()) {
        Some("splt") => write_splt(&cloud, quant)?,
        Some("spz") => write_spz(&cloud)?,
        Some("ply") => write_ply(&cloud),
        _ => bail!("output must end in .splt, .spz or .ply"),
    };
    std::fs::write(output, &bytes).with_context(|| format!("writing {}", output.display()))?;
    println!(
        "wrote {} ({} splats, {} bytes)",
        output.display(),
        cloud.len(),
        bytes.len()
    );
    Ok(())
}

/// Prints a summary of a splat file.
pub fn probe(input: &Path) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let format = detect(&bytes)
        .with_context(|| format!("{}: not a recognised splat file", input.display()))?;
    println!("format:   {}", format.name());
    println!("size:     {} bytes", bytes.len());
    match format {
        Format::Spls => {
            let seq = read_sequence(&bytes)?;
            let entries = seq.entries();
            let splats: u64 = entries.iter().map(|e| u64::from(e.members)).sum();
            println!("sh degree: {}", seq.sh_degree());
            println!(
                "slabs:    {} of {} time units",
                entries.len(),
                seq.slab_duration()
            );
            if let (Some(first), Some(last)) = (entries.first(), entries.last()) {
                println!("time:     {} .. {}", first.t_start, last.t_end);
            }
            println!("splat slab entries: {splats} (a splat may appear in several slabs)");
        }
        _ => {
            let cloud = load_cloud(input)?;
            println!("splats:   {}", cloud.len());
            println!("sh degree: {}", cloud.sh_degree());
            println!(
                "frame:    {}",
                match cloud.frame() {
                    CoordinateFrame::Rdf => "RDF (3DGS/PLY convention)",
                    CoordinateFrame::Rub => "RUB (SPZ native)",
                }
            );
        }
    }
    Ok(())
}
