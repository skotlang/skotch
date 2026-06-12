//! Minimal binary AndroidManifest.xml reader, enough to recover the two
//! facts signing needs: the effective `minSdkVersion` and whether the app is
//! `android:debuggable` (`ApkUtils` + `AndroidBinXmlParser`).
//!
//! Attributes are matched purely by their Android resource ID, which is what
//! apksig does, so the string pool only needs to be parsed far enough to read
//! the resource-map chunk.

use crate::zip::{u16le, u32le};
use anyhow::{bail, Result};

const TYPE_STRING_POOL: u16 = 0x0001;
const TYPE_RES_XML: u16 = 0x0003;
const RES_XML_START_ELEMENT: u16 = 0x0102;
const RES_XML_RESOURCE_MAP: u16 = 0x0180;

const MIN_SDK_VERSION_ATTR_ID: u32 = 0x0101_020c;
const DEBUGGABLE_ATTR_ID: u32 = 0x0101_000f;

// Res_value data types.
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
const TYPE_INT_BOOLEAN: u8 = 0x12;

struct Axml {
    /// Resource id for each string-pool index referenced as an attribute name.
    resource_map: Vec<u32>,
    /// Parsed start-element attribute list: (name_index, data_type, data).
    attributes: Vec<(u32, u8, u32)>,
}

fn parse(manifest: &[u8]) -> Result<Axml> {
    if manifest.len() < 8 || u16le(manifest, 0) != TYPE_RES_XML {
        bail!("Not a binary AndroidManifest.xml resource");
    }
    let mut resource_map: Vec<u32> = Vec::new();
    let mut attributes: Vec<(u32, u8, u32)> = Vec::new();

    // Walk the top-level RES_XML chunk's children.
    let header_size = u16le(manifest, 2) as usize;
    let mut pos = header_size;
    while pos + 8 <= manifest.len() {
        let chunk_type = u16le(manifest, pos);
        let chunk_size = u32le(manifest, pos + 4) as usize;
        if chunk_size < 8 || pos + chunk_size > manifest.len() {
            break;
        }
        match chunk_type {
            TYPE_STRING_POOL => {} // not needed: attributes matched by resource id
            RES_XML_RESOURCE_MAP => {
                let count = (chunk_size - 8) / 4;
                for i in 0..count {
                    resource_map.push(u32le(manifest, pos + 8 + i * 4));
                }
            }
            RES_XML_START_ELEMENT => {
                parse_start_element(&manifest[pos..pos + chunk_size], &mut attributes);
            }
            _ => {}
        }
        pos += chunk_size;
    }
    Ok(Axml {
        resource_map,
        attributes,
    })
}

fn parse_start_element(chunk: &[u8], attributes: &mut Vec<(u32, u8, u32)>) {
    // node header (16) then attrExt:
    //   ns u32, name u32, attributeStart u16, attributeSize u16,
    //   attributeCount u16, idIndex u16, classIndex u16, styleIndex u16
    let node_header_size = u16le(chunk, 2) as usize;
    if chunk.len() < node_header_size + 20 {
        return;
    }
    let attr_start = u16le(chunk, node_header_size + 8) as usize;
    let attr_size = u16le(chunk, node_header_size + 10) as usize;
    let attr_count = u16le(chunk, node_header_size + 12) as usize;
    let base = node_header_size + attr_start;
    for i in 0..attr_count {
        let off = base + i * attr_size;
        if off + 20 > chunk.len() {
            break;
        }
        let name_index = u32le(chunk, off + 4);
        // typedValue at off+12: size u16, res0 u8, dataType u8, data u32
        let data_type = chunk[off + 15];
        let data = u32le(chunk, off + 16);
        attributes.push((name_index, data_type, data));
    }
}

impl Axml {
    fn attr_resource_id(&self, name_index: u32) -> Option<u32> {
        self.resource_map.get(name_index as usize).copied()
    }
}

/// Returns the effective `minSdkVersion` (max over all `uses-sdk` declarations,
/// default 1). Codename-only minSdkVersion is not supported and yields an error.
pub fn min_sdk_version(manifest: &[u8]) -> Result<u32> {
    let axml = parse(manifest)?;
    let mut result = 1u32;
    for &(name_index, data_type, data) in &axml.attributes {
        if axml.attr_resource_id(name_index) == Some(MIN_SDK_VERSION_ATTR_ID) {
            match data_type {
                TYPE_INT_DEC | TYPE_INT_HEX => result = result.max(data),
                _ => bail!(
                    "Unable to determine APK's minimum supported platform version: \
                     unsupported android:minSdkVersion value type"
                ),
            }
        }
    }
    Ok(result)
}

/// Returns whether the application declares `android:debuggable="true"`.
pub fn is_debuggable(manifest: &[u8]) -> Result<bool> {
    let axml = parse(manifest)?;
    for &(name_index, data_type, data) in &axml.attributes {
        if axml.attr_resource_id(name_index) == Some(DEBUGGABLE_ATTR_ID) {
            match data_type {
                TYPE_INT_BOOLEAN | TYPE_INT_DEC | TYPE_INT_HEX => return Ok(data != 0),
                _ => bail!(
                    "Unable to determine whether APK is debuggable: \
                     unsupported android:debuggable value type"
                ),
            }
        }
    }
    Ok(false)
}
