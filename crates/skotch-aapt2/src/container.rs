//! AAPT2 Container Format (`.apc` / `.flat`) reader and writer.
//!
//! Port of `format/Container.{h,cpp}`. The container is the output of
//! `aapt2 compile` and the input of `aapt2 link`: a sequence of entries,
//! each either a serialized `aapt.pb.ResourceTable` (RES_TABLE) or a
//! compiled file (RES_FILE: a `aapt.pb.internal.CompiledFile` header
//! followed by PNG/binary-XML/proto-XML payload).
//!
//! Layout (all little-endian, see `formats.md`):
//!
//! ```text
//! u32 magic = 0x54504141 ('AAPT')
//! u32 version = 1
//! u32 entry_count
//! entry*:
//!   u32 entry_type        (0 = RES_TABLE, 1 = RES_FILE)
//!   u64 entry_length
//!   data[entry_length]    (+ padding to 4-byte alignment)
//! ```
//!
//! RES_FILE data:
//!
//! ```text
//! u32 header_size
//! u64 data_size
//! header[header_size]     pb::internal::CompiledFile (+ pad to 4)
//! data[data_size]         payload (+ pad to 4)
//! ```

use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Read};

pub const CONTAINER_MAGIC: u32 = 0x54504141; // 'AAPT'
pub const CONTAINER_VERSION: u32 = 1;

pub const ENTRY_RES_TABLE: u32 = 0;
pub const ENTRY_RES_FILE: u32 = 1;

/// One parsed container entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerEntry {
    /// A serialized `aapt.pb.ResourceTable`.
    ResTable { table_pb: Vec<u8> },
    /// A compiled file: serialized `aapt.pb.internal.CompiledFile`
    /// header plus raw payload.
    ResFile {
        compiled_file_pb: Vec<u8>,
        data: Vec<u8>,
    },
}

/// Writes a container to an in-memory buffer.
pub struct ContainerWriter {
    buf: Vec<u8>,
    declared_entries: u32,
    written_entries: u32,
}

impl ContainerWriter {
    pub fn new(entry_count: usize) -> Self {
        let mut buf = Vec::new();
        buf.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
        buf.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
        buf.extend_from_slice(&(entry_count as u32).to_le_bytes());
        ContainerWriter {
            buf,
            declared_entries: entry_count as u32,
            written_entries: 0,
        }
    }

    fn align4(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }

    /// Adds a RES_TABLE entry holding a serialized
    /// `aapt.pb.ResourceTable`.
    pub fn add_res_table(&mut self, table_pb: &[u8]) {
        self.align4();
        self.buf.extend_from_slice(&ENTRY_RES_TABLE.to_le_bytes());
        self.buf
            .extend_from_slice(&(table_pb.len() as u64).to_le_bytes());
        self.buf.extend_from_slice(table_pb);
        self.written_entries += 1;
    }

    /// Adds a RES_FILE entry: a serialized
    /// `aapt.pb.internal.CompiledFile` plus the file payload.
    pub fn add_res_file(&mut self, compiled_file_pb: &[u8], data: &[u8]) {
        self.align4();
        let header_padding = (4 - compiled_file_pb.len() % 4) % 4;
        let data_padding = (4 - data.len() % 4) % 4;
        let entry_length =
            4 + 8 + compiled_file_pb.len() + header_padding + data.len() + data_padding;

        self.buf.extend_from_slice(&ENTRY_RES_FILE.to_le_bytes());
        self.buf
            .extend_from_slice(&(entry_length as u64).to_le_bytes());
        self.buf
            .extend_from_slice(&(compiled_file_pb.len() as u32).to_le_bytes());
        self.buf
            .extend_from_slice(&(data.len() as u64).to_le_bytes());
        self.buf.extend_from_slice(compiled_file_pb);
        self.buf.extend_from_slice(&vec![0u8; header_padding]);
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(&vec![0u8; data_padding]);
        self.written_entries += 1;
    }

    pub fn finish(self) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(
            self.written_entries == self.declared_entries,
            "container declared {} entries but {} were written",
            self.declared_entries,
            self.written_entries
        );
        Ok(self.buf)
    }
}

/// Parses every entry of a container file.
pub fn read_container(data: &[u8]) -> anyhow::Result<Vec<ContainerEntry>> {
    let mut cursor = Cursor::new(data);
    let magic = cursor.read_u32::<LittleEndian>()?;
    anyhow::ensure!(
        magic == CONTAINER_MAGIC,
        "magic value is 0x{magic:08x} but AAPT expects 0x{CONTAINER_MAGIC:08x}"
    );
    let version = cursor.read_u32::<LittleEndian>()?;
    anyhow::ensure!(
        version <= CONTAINER_VERSION,
        "container version is 0x{version:08x} but AAPT expects version 0x{CONTAINER_VERSION:08x} or lower"
    );
    let entry_count = cursor.read_u32::<LittleEndian>()?;

    let mut entries = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        // Entries are aligned on 4-byte boundaries.
        let pos = cursor.position();
        if !pos.is_multiple_of(4) {
            cursor.set_position(pos + (4 - pos % 4));
        }
        if cursor.position() as usize >= data.len() {
            break;
        }

        let entry_type = cursor.read_u32::<LittleEndian>()?;
        let entry_length = cursor.read_u64::<LittleEndian>()?;
        match entry_type {
            ENTRY_RES_TABLE => {
                let mut table_pb = vec![0u8; entry_length as usize];
                cursor.read_exact(&mut table_pb)?;
                entries.push(ContainerEntry::ResTable { table_pb });
            }
            ENTRY_RES_FILE => {
                let header_size = cursor.read_u32::<LittleEndian>()?;
                let data_size = cursor.read_u64::<LittleEndian>()?;
                let mut compiled_file_pb = vec![0u8; header_size as usize];
                cursor.read_exact(&mut compiled_file_pb)?;
                let header_padding = (4 - header_size as u64 % 4) % 4;
                cursor.set_position(cursor.position() + header_padding);
                let mut payload = vec![0u8; data_size as usize];
                cursor.read_exact(&mut payload)?;
                let data_padding = (4 - data_size % 4) % 4;
                cursor.set_position(cursor.position() + data_padding);
                entries.push(ContainerEntry::ResFile {
                    compiled_file_pb,
                    data: payload,
                });
            }
            other => anyhow::bail!("entry type 0x{other:08x} is invalid"),
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_mixed_entries() {
        let table_pb = vec![1u8, 2, 3];
        let file_pb = vec![9u8; 5]; // odd length to exercise padding
        let payload = vec![7u8; 6];

        let mut writer = ContainerWriter::new(2);
        writer.add_res_table(&table_pb);
        writer.add_res_file(&file_pb, &payload);
        let bytes = writer.finish().unwrap();

        let entries = read_container(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ContainerEntry::ResTable { table_pb });
        assert_eq!(
            entries[1],
            ContainerEntry::ResFile {
                compiled_file_pb: file_pb,
                data: payload
            }
        );
    }

    #[test]
    fn bad_magic_rejected() {
        let err = read_container(&[0u8; 12]).unwrap_err();
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn entry_count_mismatch_rejected() {
        let writer = ContainerWriter::new(1);
        assert!(writer.finish().is_err());
    }
}
