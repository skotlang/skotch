//! JAR (== ZIP with a META-INF/MANIFEST.MF) packaging.

use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

/// Write a runnable JAR with the given main class and the given
/// `(internal_name, bytes)` `.class` payloads.
pub fn write_jar(
    output: &Path,
    main_class: &str,
    classes: &[(String, Vec<u8>)],
) -> std::io::Result<()> {
    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let manifest = format!(
        "Manifest-Version: 1.0\r\nMain-Class: {}\r\n\r\n",
        main_class.replace('/', ".")
    );
    zip.start_file("META-INF/MANIFEST.MF", opts)?;
    zip.write_all(manifest.as_bytes())?;

    for (internal_name, bytes) in classes {
        let entry_name = format!("{internal_name}.class");
        zip.start_file(entry_name, opts)?;
        zip.write_all(bytes)?;
    }

    zip.finish()?;
    Ok(())
}

/// Write a JAR to an in-memory buffer.
pub fn write_jar_to_vec(
    main_class: &str,
    classes: &[(String, Vec<u8>)],
) -> std::io::Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let manifest = format!(
        "Manifest-Version: 1.0\r\nMain-Class: {}\r\n\r\n",
        main_class.replace('/', ".")
    );
    zip.start_file("META-INF/MANIFEST.MF", opts)?;
    zip.write_all(manifest.as_bytes())?;

    for (internal_name, bytes) in classes {
        let entry_name = format!("{internal_name}.class");
        zip.start_file(entry_name, opts)?;
        zip.write_all(bytes)?;
    }

    zip.finish()?;
    Ok(buf.into_inner())
}
