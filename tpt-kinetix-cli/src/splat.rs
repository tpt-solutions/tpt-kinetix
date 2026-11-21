//! `tpt-kinetix splat`: convert and inspect Gaussian splat files.
//!
//! Formats are recognised by their magic bytes, not their extensions:
//! PLY (`ply`), SPZ (gzip `1f 8b` or `NGSP`), `SPLT` (static 3D), `SPLS`
//! (4D sequence), and MP4 holding an `spls` track (a sequence, one sample per
//! time slab).

use std::path::Path;

use anyhow::{bail, Context, Result};
use tpt_kinetix_ops::{mp4_to_spls, spls_to_mp4};
use tpt_kinetix_splat::{
    read_ply, read_sequence, read_splt, read_spz, write_glb, write_ply, write_splt, write_spz,
    CoordinateFrame, GaussianCloud, Quantization,
};

/// A splat file's format, from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ply,
    Spz,
    Splt,
    Spls,
    /// An MP4 whose `spls` track holds a 4D sequence.
    Mp4Spls,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Ply => "PLY (3D Gaussian splats)",
            Format::Spz => "SPZ (3D Gaussian splats)",
            Format::Splt => "SPLT (3D Gaussian splats, native codec)",
            Format::Spls => "SPLS (4D Gaussian splat sequence)",
            Format::Mp4Spls => "MP4 with an spls track (4D Gaussian splat sequence)",
        }
    }

    /// `true` for 4D sequence formats (SPLS, or MP4 carrying one).
    fn is_sequence(self) -> bool {
        matches!(self, Format::Spls | Format::Mp4Spls)
    }
}

/// Recognises a splat format from the first bytes of a file.
///
/// MP4 is recognised by its `ftyp` box, then confirmed by looking for an
/// `spls` track; an ordinary MP4 is not a splat file.
pub fn detect(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"ply\n") || bytes.starts_with(b"ply\r\n") {
        Some(Format::Ply)
    } else if bytes.starts_with(&[0x1f, 0x8b]) || bytes.starts_with(b"NGSP") {
        Some(Format::Spz)
    } else if bytes.starts_with(b"SPLT") {
        Some(Format::Splt)
    } else if bytes.starts_with(b"SPLS") {
        Some(Format::Spls)
    } else if bytes.get(4..8) == Some(b"ftyp") && mp4_to_spls(bytes).is_ok() {
        Some(Format::Mp4Spls)
    } else {
        None
    }
}

/// The `SPLS` bytes of a sequence file, whether it is `SPLS` or MP4.
fn sequence_bytes(format: Format, bytes: &[u8]) -> Result<Vec<u8>> {
    match format {
        Format::Spls => Ok(bytes.to_vec()),
        Format::Mp4Spls => mp4_to_spls(bytes),
        _ => bail!("not a 4D sequence"),
    }
}

/// Reads a static 3D cloud from a PLY, SPZ or SPLT file.
fn load_cloud(path: &Path) -> Result<GaussianCloud> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    match detect(&bytes) {
        Some(Format::Ply) => Ok(read_ply(&bytes)?),
        Some(Format::Spz) => Ok(read_spz(&bytes)?),
        Some(Format::Splt) => Ok(read_splt(&bytes)?),
        Some(f) if f.is_sequence() => {
            bail!("{} is a 4D sequence, not a static cloud", path.display())
        }
        None => bail!("{}: not a recognised splat file", path.display()),
        Some(_) => unreachable!("every format is handled above"),
    }
}

/// Converts between splat files.
///
/// * Static clouds convert to `.splt` (native codec), `.spz` or `.ply`.
/// * Sequences (`SPLS` or MP4) convert to `.mp4` (`spls` track) or `.spls`.
pub fn convert(input: &Path, output: &Path, quant: Quantization) -> Result<()> {
    let out_ext = output.extension().and_then(|e| e.to_str());
    let in_bytes = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let format = detect(&in_bytes)
        .with_context(|| format!("{}: not a recognised splat file", input.display()))?;

    if format.is_sequence() {
        let spls = sequence_bytes(format, &in_bytes)?;
        let bytes = match out_ext {
            Some("mp4") => spls_to_mp4(&spls)?,
            Some("spls") => spls,
            _ => bail!("a sequence converts to .mp4 or .spls"),
        };
        std::fs::write(output, &bytes).with_context(|| format!("writing {}", output.display()))?;
        println!("wrote {} ({} bytes)", output.display(), bytes.len());
        return Ok(());
    }

    let cloud = load_cloud(input)?;
    let bytes = match out_ext {
        Some("splt") => write_splt(&cloud, quant)?,
        Some("spz") => write_spz(&cloud)?,
        Some("ply") => write_ply(&cloud),
        Some("glb") => write_glb(&cloud)?,
        _ => bail!("output must end in .splt, .spz, .ply or .glb (or .mp4/.spls for sequences)"),
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
    if format.is_sequence() {
        let spls = sequence_bytes(format, &bytes)?;
        let seq = read_sequence(&spls)?;
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
    } else {
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
    Ok(())
}
