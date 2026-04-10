//! APK (Android Package) assembly.
//!
//! An APK is a ZIP file with specific structure and alignment:
//!
//! - `AndroidManifest.xml` (binary AXML, STORED)
//! - `classes.dex` (DEX bytecode, STORED)
//! - `resources.arsc` (compiled resource table, optional)
//! - `res/` directory entries
//!
//! Uncompressed entries must be 4-byte aligned. The signing block
//! (handled by `skotch-sign`) is inserted between the ZIP entries
//! and the central directory after assembly.

use byteorder::{LittleEndian, WriteBytesExt};
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

/// Contents to bundle into an APK.
pub struct ApkContents {
    /// Binary AXML for AndroidManifest.xml (from `skotch-axml`).
    pub manifest_xml: Vec<u8>,
    /// DEX bytecode (from `skotch-backend-dex`).
    pub classes_dex: Vec<u8>,
    /// Compiled resource table. `None` for minimal APKs with no resources.
    pub resources_arsc: Option<Vec<u8>>,
    /// Additional resource files: `(path_in_apk, data)`.
    pub res_files: Vec<(String, Vec<u8>)>,
}

/// Assemble an unsigned APK and write it to disk.
pub fn write_unsigned_apk(output: &Path, contents: &ApkContents) -> std::io::Result<()> {
    let bytes = assemble_unsigned_apk(contents)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, bytes)
}

/// Assemble an unsigned APK as a byte vector.
pub fn assemble_unsigned_apk(contents: &ApkContents) -> std::io::Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);

    // STORED (no compression) with 4-byte alignment for binary entries.
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .with_alignment(4);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // 1. AndroidManifest.xml (STORED, aligned)
    zip.start_file("AndroidManifest.xml", stored)?;
    zip.write_all(&contents.manifest_xml)?;

    // 2. classes.dex (STORED, aligned)
    zip.start_file("classes.dex", stored)?;
    zip.write_all(&contents.classes_dex)?;

    // 3. resources.arsc (STORED, aligned) if present
    if let Some(arsc) = &contents.resources_arsc {
        zip.start_file("resources.arsc", stored)?;
        zip.write_all(arsc)?;
    }

    // 4. res/ files (DEFLATED)
    for (path, data) in &contents.res_files {
        zip.start_file(path, deflated)?;
        zip.write_all(data)?;
    }

    zip.finish()?;
    Ok(buf.into_inner())
}

/// Insert an APK signing block between the ZIP entries and the central
/// directory. Returns the modified APK bytes.
///
/// The signing block sits right before the central directory, and the
/// EOCD's "offset of start of central directory" field is updated to
/// account for the inserted block.
pub fn insert_signing_block(unsigned_apk: &[u8], signing_block: &[u8]) -> std::io::Result<Vec<u8>> {
    // Find the End of Central Directory (EOCD) record by scanning
    // backwards for the EOCD signature 0x06054b50.
    let eocd_sig: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
    let eocd_offset = unsigned_apk
        .windows(4)
        .rposition(|w| w == eocd_sig)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "EOCD not found"))?;

    // Read the central directory offset from EOCD (at EOCD + 16).
    let cd_offset = u32::from_le_bytes([
        unsigned_apk[eocd_offset + 16],
        unsigned_apk[eocd_offset + 17],
        unsigned_apk[eocd_offset + 18],
        unsigned_apk[eocd_offset + 19],
    ]) as usize;

    // Build the output:
    //   [ZIP entries] [signing block] [central directory] [patched EOCD]
    let mut out = Vec::with_capacity(unsigned_apk.len() + signing_block.len());

    // ZIP entries (up to the central directory).
    out.write_all(&unsigned_apk[..cd_offset])?;

    // Signing block.
    out.write_all(signing_block)?;

    // Central directory (unchanged).
    out.write_all(&unsigned_apk[cd_offset..eocd_offset])?;

    // EOCD with patched central directory offset.
    out.write_all(&unsigned_apk[eocd_offset..eocd_offset + 16])?;
    let new_cd_offset = (cd_offset + signing_block.len()) as u32;
    out.write_u32::<LittleEndian>(new_cd_offset)?;
    out.write_all(&unsigned_apk[eocd_offset + 20..])?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_minimal_apk() {
        let contents = ApkContents {
            manifest_xml: vec![0x03, 0x00, 0x08, 0x00], // minimal header
            classes_dex: vec![0x64, 0x65, 0x78, 0x0A],  // "dex\n" magic
            resources_arsc: None,
            res_files: vec![],
        };
        let apk = assemble_unsigned_apk(&contents).unwrap();
        // Verify it's a valid ZIP (starts with PK signature).
        assert_eq!(&apk[0..2], b"PK");
        // Verify entries exist by checking ZIP has multiple local headers.
        assert!(apk.len() > 100);
    }

    #[test]
    fn apk_contains_expected_entries() {
        let contents = ApkContents {
            manifest_xml: b"manifest-data".to_vec(),
            classes_dex: b"dex-data".to_vec(),
            resources_arsc: Some(b"arsc-data".to_vec()),
            res_files: vec![("res/values/strings.xml".into(), b"<resources/>".to_vec())],
        };
        let apk = assemble_unsigned_apk(&contents).unwrap();
        let reader = std::io::Cursor::new(&apk);
        let mut archive = zip::ZipArchive::new(reader).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"AndroidManifest.xml".to_string()));
        assert!(names.contains(&"classes.dex".to_string()));
        assert!(names.contains(&"resources.arsc".to_string()));
        assert!(names.contains(&"res/values/strings.xml".to_string()));
    }
}
