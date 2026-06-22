//! PNG processing for `aapt2 compile`.
//!
//! Port of the androidfw PNG pipeline used by `CompilePng`:
//! `PngChunkFilter` (strip non-essential chunks) and, for `.9.png`
//! inputs, nine-patch processing (`NinePatch::Create` + `WritePng`
//! with the `npTc`/`npLb`/`npOl` chunks).
//!
//! Current state: chunk filtering is fully implemented; recompression
//! ("crunching") is not performed, matching aapt2's behavior whenever
//! the recompressed image would be larger than the source (aapt2 then
//! ships the chunk-filtered original — byte-identical to what this
//! module produces). Nine-patch processing is implemented in
//! [`process_nine_patch`].

use anyhow::{anyhow, bail, Result};

pub const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

const CHUNK_IHDR: &[u8; 4] = b"IHDR";
const CHUNK_IDAT: &[u8; 4] = b"IDAT";
const CHUNK_IEND: &[u8; 4] = b"IEND";
const CHUNK_PLTE: &[u8; 4] = b"PLTE";
const CHUNK_TRNS: &[u8; 4] = b"tRNS";
const CHUNK_SRGB: &[u8; 4] = b"sRGB";

fn is_chunk_allowed(chunk_type: &[u8]) -> bool {
    matches!(
        chunk_type,
        t if t == CHUNK_IHDR
            || t == CHUNK_IDAT
            || t == CHUNK_IEND
            || t == CHUNK_PLTE
            || t == CHUNK_TRNS
            || t == CHUNK_SRGB
    )
}

/// Walks the chunks of `data`, passing each to `f` as
/// `(type, full-chunk-bytes-including-length-and-crc)`.
fn for_each_chunk(data: &[u8], mut f: impl FnMut(&[u8], &[u8]) -> Result<()>) -> Result<()> {
    if data.len() < PNG_SIGNATURE.len() || data[..8] != PNG_SIGNATURE {
        bail!("file does not start with PNG signature");
    }
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let chunk_type = &data[offset + 4..offset + 8];
        let total = 8 + length + 4;
        if offset + total > data.len() {
            bail!("PNG chunk is truncated");
        }
        f(chunk_type, &data[offset..offset + total])?;
        if chunk_type == CHUNK_IEND {
            return Ok(());
        }
        offset += total;
    }
    bail!("PNG is missing IEND chunk");
}

/// Strips all non-essential chunks, keeping IHDR/PLTE/tRNS/sRGB/IDAT/
/// IEND. Port of `android::PngChunkFilter`.
pub fn filter_chunks(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&PNG_SIGNATURE);
    for_each_chunk(data, |chunk_type, chunk| {
        if is_chunk_allowed(chunk_type) {
            out.extend_from_slice(chunk);
        }
        Ok(())
    })?;
    Ok(out)
}

/// Processes a regular PNG: today this is chunk filtering only (see the
/// module docs for why this matches aapt2's output in the common case).
pub fn crunch_png(data: &[u8], _compression_level: u8) -> Result<Vec<u8>> {
    filter_chunks(data)
}

/// Decoded RGBA image.
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Row-major RGBA, `height` rows of `width * 4` bytes.
    pub pixels: Vec<u8>,
}

impl Image {
    pub fn row(&self, y: usize) -> &[u8] {
        &self.pixels[y * self.width * 4..(y + 1) * self.width * 4]
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * self.width + x) * 4;
        self.pixels[offset..offset + 4].try_into().unwrap()
    }
}

/// Processes a nine-patch PNG: decodes the image, derives the `npTc`
/// (and optionally `npLb`/`npOl`) chunks from the 1px border, strips
/// the border, and re-encodes. Port of `NinePatch::Create` + `WritePng`.
pub fn process_nine_patch(data: &[u8], compression_level: u8) -> Result<Vec<u8>> {
    let image = decode_png(data)?;
    if image.width < 3 || image.height < 3 {
        bail!("image must be at least 3x3 (1x1 image with 1 pixel border)");
    }
    let nine_patch = crate::compile::ninepatch::create(&image).map_err(|e| anyhow!(e))?;

    // Strip the 1px border.
    let inner_width = image.width - 2;
    let inner_height = image.height - 2;
    let mut pixels = Vec::with_capacity(inner_width * inner_height * 4);
    for y in 1..image.height - 1 {
        let row = image.row(y);
        pixels.extend_from_slice(&row[4..4 + inner_width * 4]);
    }
    let inner = Image {
        width: inner_width,
        height: inner_height,
        pixels,
    };

    encode_png(&inner, Some(&nine_patch), compression_level)
}

/// Decodes a PNG into RGBA8. Supports the color types and bit depths
/// emitted by image editors for resources (grayscale, RGB, palette,
/// grayscale+alpha, RGBA; 8-bit; plus tRNS).
pub fn decode_png(data: &[u8]) -> Result<Image> {
    let mut ihdr: Option<(usize, usize, u8, u8, u8)> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();

    for_each_chunk(data, |chunk_type, chunk| {
        let payload = &chunk[8..chunk.len() - 4];
        match chunk_type {
            t if t == CHUNK_IHDR => {
                if payload.len() < 13 {
                    bail!("IHDR too short");
                }
                let width = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as usize;
                let height = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize;
                let bit_depth = payload[8];
                let color_type = payload[9];
                let interlace = payload[12];
                ihdr = Some((width, height, bit_depth, color_type, interlace));
            }
            t if t == CHUNK_PLTE => {
                palette = payload
                    .chunks_exact(3)
                    .map(|c| [c[0], c[1], c[2]])
                    .collect();
            }
            t if t == CHUNK_TRNS => trns = payload.to_vec(),
            t if t == CHUNK_IDAT => idat.extend_from_slice(payload),
            _ => {}
        }
        Ok(())
    })?;

    let (width, height, bit_depth, color_type, interlace) =
        ihdr.ok_or_else(|| anyhow!("PNG is missing IHDR"))?;
    if interlace != 0 {
        bail!("interlaced PNGs are not supported");
    }
    if width == 0 || height == 0 {
        bail!("invalid image dimensions");
    }

    // Inflate the IDAT stream.
    let mut raw = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(idat.as_slice());
    std::io::Read::read_to_end(&mut decoder, &mut raw)?;

    let channels: usize = match color_type {
        0 => 1, // grayscale
        2 => 3, // rgb
        3 => 1, // palette
        4 => 2, // grayscale + alpha
        6 => 4, // rgba
        other => bail!("unsupported PNG color type {other}"),
    };
    if !matches!(bit_depth, 1 | 2 | 4 | 8) || (bit_depth != 8 && color_type != 3 && color_type != 0)
    {
        bail!("unsupported PNG bit depth {bit_depth} for color type {color_type}");
    }

    let bits_per_pixel = channels * bit_depth as usize;
    let stride = (width * bits_per_pixel).div_ceil(8);
    let expected = (stride + 1) * height;
    if raw.len() < expected {
        bail!("PNG pixel data is truncated ({} < {expected})", raw.len());
    }

    // Undo per-row filters.
    let bpp = bits_per_pixel.div_ceil(8).max(1);
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(height);
    for y in 0..height {
        let row_start = y * (stride + 1);
        let filter = raw[row_start];
        let mut row = raw[row_start + 1..row_start + 1 + stride].to_vec();
        let prev = y.checked_sub(1).map(|p| rows[p].as_slice());
        unfilter_row(filter, &mut row, prev, bpp)?;
        rows.push(row);
    }

    // Expand to RGBA8.
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in &rows {
        match color_type {
            6 => pixels.extend_from_slice(&row[..width * 4]),
            2 => {
                for x in 0..width {
                    pixels.extend_from_slice(&row[x * 3..x * 3 + 3]);
                    pixels.push(0xff);
                }
            }
            4 => {
                for x in 0..width {
                    let g = row[x * 2];
                    pixels.extend_from_slice(&[g, g, g, row[x * 2 + 1]]);
                }
            }
            0 => {
                for x in 0..width {
                    let g = read_sub_byte(row, x, bit_depth, width);
                    let g8 = scale_to_8bit(g, bit_depth);
                    pixels.extend_from_slice(&[g8, g8, g8, 0xff]);
                }
            }
            3 => {
                for x in 0..width {
                    let index = read_sub_byte(row, x, bit_depth, width) as usize;
                    let rgb = palette.get(index).copied().unwrap_or([0, 0, 0]);
                    let alpha = trns.get(index).copied().unwrap_or(0xff);
                    pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
                }
            }
            _ => unreachable!(),
        }
    }

    Ok(Image {
        width,
        height,
        pixels,
    })
}

fn read_sub_byte(row: &[u8], x: usize, bit_depth: u8, _width: usize) -> u8 {
    match bit_depth {
        8 => row[x],
        4 => (row[x / 2] >> (4 * (1 - (x % 2)))) & 0x0f,
        2 => (row[x / 4] >> (2 * (3 - (x % 4)))) & 0x03,
        1 => (row[x / 8] >> (7 - (x % 8))) & 0x01,
        _ => 0,
    }
}

fn scale_to_8bit(value: u8, bit_depth: u8) -> u8 {
    match bit_depth {
        8 => value,
        4 => value * 17,
        2 => value * 85,
        1 => value * 255,
        _ => value,
    }
}

fn unfilter_row(filter: u8, row: &mut [u8], prev: Option<&[u8]>, bpp: usize) -> Result<()> {
    match filter {
        0 => {}
        1 => {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        2 => {
            if let Some(prev) = prev {
                for i in 0..row.len() {
                    row[i] = row[i].wrapping_add(prev[i]);
                }
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= bpp { row[i - bpp] as u16 } else { 0 };
                let up = prev.map(|p| p[i] as u16).unwrap_or(0);
                row[i] = row[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..row.len() {
                let left = if i >= bpp { row[i - bpp] as i16 } else { 0 };
                let up = prev.map(|p| p[i] as i16).unwrap_or(0);
                let up_left = if i >= bpp {
                    prev.map(|p| p[i - bpp] as i16).unwrap_or(0)
                } else {
                    0
                };
                let p = left + up - up_left;
                let pa = (p - left).abs();
                let pb = (p - up).abs();
                let pc = (p - up_left).abs();
                let predictor = if pa <= pb && pa <= pc {
                    left
                } else if pb <= pc {
                    up
                } else {
                    up_left
                };
                row[i] = row[i].wrapping_add(predictor as u8);
            }
        }
        other => bail!("invalid PNG filter type {other}"),
    }
    Ok(())
}

/// Encodes an RGBA image as a PNG, embedding serialized nine-patch
/// chunks when given. Rows are filtered with filter 0 (none); the
/// runtime only requires a valid PNG, and aapt2's own size-based
/// original-vs-crunched choice makes byte parity here non-normative.
pub fn encode_png(
    image: &Image,
    nine_patch: Option<&crate::compile::ninepatch::NinePatch>,
    compression_level: u8,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&PNG_SIGNATURE);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(image.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(image.height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Nine-patch chunks come before IDAT, matching WritePng.
    if let Some(nine_patch) = nine_patch {
        write_chunk(&mut out, b"npOl", &nine_patch.serialize_outline());
        if let Some(layout_bounds) = nine_patch.serialize_layout_bounds() {
            write_chunk(&mut out, b"npLb", &layout_bounds);
        }
        write_chunk(&mut out, b"npTc", &nine_patch.serialize_tc());
    }

    let mut raw = Vec::with_capacity((image.width * 4 + 1) * image.height);
    for y in 0..image.height {
        raw.push(0); // filter type none
        raw.extend_from_slice(image.row(y));
    }
    let mut encoder = flate2::write::ZlibEncoder::new(
        Vec::new(),
        flate2::Compression::new(compression_level.min(9) as u32),
    );
    std::io::Write::write_all(&mut encoder, &raw)?;
    let compressed = encoder.finish()?;
    write_chunk(&mut out, b"IDAT", &compressed);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(payload);
    let crc = crc32(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// CRC-32 (IEEE) over the chunk type + data, per the PNG spec.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_png() -> Vec<u8> {
        // 3x3 red square with an extra text chunk to be filtered.
        let image = Image {
            width: 3,
            height: 3,
            pixels: [[255u8, 0, 0, 255]; 9].concat(),
        };
        let mut png = encode_png(&image, None, 9).unwrap();
        // Inject a tEXt chunk before IEND.
        let iend_offset = png.len() - 12;
        let mut text_chunk = Vec::new();
        write_chunk(&mut text_chunk, b"tEXt", b"comment\0hello");
        png.splice(iend_offset..iend_offset, text_chunk);
        png
    }

    #[test]
    fn filter_strips_text_chunks() {
        let png = tiny_png();
        assert!(png.windows(4).any(|w| w == b"tEXt"));
        let filtered = filter_chunks(&png).unwrap();
        assert!(!filtered.windows(4).any(|w| w == b"tEXt"));
        // Still decodable.
        let image = decode_png(&filtered).unwrap();
        assert_eq!((image.width, image.height), (3, 3));
        assert_eq!(image.pixel(1, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_png(b"not a png").is_err());
        assert!(filter_chunks(&[0u8; 64]).is_err());
    }

    #[test]
    fn png_round_trip() {
        let image = Image {
            width: 2,
            height: 2,
            pixels: vec![
                1, 2, 3, 4, 5, 6, 7, 8, //
                9, 10, 11, 12, 13, 14, 15, 16,
            ],
        };
        let png = encode_png(&image, None, 6).unwrap();
        let decoded = decode_png(&png).unwrap();
        assert_eq!(decoded.pixels, image.pixels);
    }
}
