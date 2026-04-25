//! JAR (== ZIP with a META-INF/MANIFEST.MF) packaging.

use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

/// Write a runnable JAR with the given main class, `.class` payloads,
/// and resource files.
pub fn write_jar(
    output: &Path,
    main_class: &str,
    classes: &[(String, Vec<u8>)],
    resources: &[(String, Vec<u8>)],
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

    for (path, bytes) in resources {
        zip.start_file(path, opts)?;
        zip.write_all(bytes)?;
    }

    zip.finish()?;
    Ok(())
}

/// Write a runnable fat JAR that includes classes from dependency JARs.
/// Dependency JAR .class files are extracted and merged into the output.
pub fn write_fat_jar(
    output: &Path,
    main_class: &str,
    classes: &[(String, Vec<u8>)],
    dep_jars: &[std::path::PathBuf],
    resources: &[(String, Vec<u8>)],
) -> std::io::Result<()> {
    use std::collections::HashSet;
    use std::io::Read;

    let file = std::fs::File::create(output)?;
    let mut zip_out = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let manifest = format!(
        "Manifest-Version: 1.0\r\nMain-Class: {}\r\n\r\n",
        main_class.replace('/', ".")
    );
    zip_out.start_file("META-INF/MANIFEST.MF", opts)?;
    zip_out.write_all(manifest.as_bytes())?;

    // Track written entries to avoid duplicates.
    let mut written: HashSet<String> = HashSet::new();
    written.insert("META-INF/MANIFEST.MF".to_string());

    // Write project's own classes.
    for (internal_name, bytes) in classes {
        let entry_name = format!("{internal_name}.class");
        if written.insert(entry_name.clone()) {
            zip_out.start_file(&entry_name, opts)?;
            zip_out.write_all(bytes)?;
        }
    }

    // Write resource files.
    for (path, bytes) in resources {
        if written.insert(path.clone()) {
            zip_out.start_file(path, opts)?;
            zip_out.write_all(bytes)?;
        }
    }

    // Extract and include .class files from dependency JARs.
    for jar_path in dep_jars {
        let jar_file = match std::fs::File::open(jar_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut archive = match zip::ZipArchive::new(jar_file) {
            Ok(a) => a,
            Err(_) => continue,
        };
        for i in 0..archive.len() {
            let mut entry = match archive.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.name().to_string();
            // Include .class files but skip META-INF (avoid manifest conflicts).
            if name.ends_with(".class") && written.insert(name.clone()) {
                let mut buf = Vec::new();
                let _ = entry.read_to_end(&mut buf);
                zip_out.start_file(&name, opts)?;
                zip_out.write_all(&buf)?;
            }
        }
    }

    zip_out.finish()?;
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
