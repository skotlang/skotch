//! Nine-patch (`.9.png`) border analysis and chunk serialization.
//!
//! Port of `android::NinePatch` from androidfw:
//! - `libs/androidfw/NinePatch.cpp` (`NinePatch::Create`, `FillRanges`,
//!   `PopulateBounds`, `CalculateSegmentCount`, `CalculateRegionColors`,
//!   `FindOutlineInsets`, `FindMaxAlpha`, and the `Serialize*` helpers).
//! - `libs/androidfw/include/androidfw/Image.h` (`Range`, `Bounds`,
//!   `NinePatch` field declarations).
//! - `libs/androidfw/include/androidfw/ResourceTypes.h` /
//!   `ResourceTypes.cpp` (`Res_png_9patch` layout, `serialize`,
//!   `fill9patchOffsets`, `deviceToFile`).
//!
//! Endianness of the serialized chunks (verified against the C++ code
//! paths used by aapt2's `WritePng` in `PngCrunch.cpp`):
//!
//! - `npTc` (`serialize_tc`): `Res_png_9patch::serialize` writes the
//!   struct in host order, `fill9patchOffsets` fills
//!   `xDivsOffset`/`yDivsOffset`/`colorsOffset` in host order, and then
//!   `NinePatch::SerializeBase` calls `Res_png_9patch::deviceToFile()`
//!   which `htonl`s ONLY the divs, padding, and colors. So on the
//!   little-endian hosts aapt2 runs on, the offsets are little-endian
//!   while padding/xDivs/yDivs/colors are big-endian. The runtime's
//!   `Res_png_9patch::deserialize` recomputes the offsets and `ntohl`s
//!   the rest, so this asymmetry is load-bearing and preserved here.
//! - `npOl` (`serialize_outline`) and `npLb` (`serialize_layout_bounds`):
//!   raw `memcpy` of host-order values, i.e. little-endian everywhere.

use crate::compile::png::Image;

// Colors in the format 0xAARRGGBB (the way 9-patch expects it).
const COLOR_OPAQUE_WHITE: u32 = 0xffff_ffff;
const COLOR_OPAQUE_BLACK: u32 = 0xff00_0000;
const COLOR_OPAQUE_RED: u32 = 0xffff_0000;

const PRIMARY_COLOR: u32 = COLOR_OPAQUE_BLACK;
const SECONDARY_COLOR: u32 = COLOR_OPAQUE_RED;

/// `Res_png_9patch::NO_COLOR`: the 9-patch segment is not a solid color.
pub const NO_COLOR: u32 = 0x0000_0001;
/// `Res_png_9patch::TRANSPARENT_COLOR`: the segment is fully transparent.
pub const TRANSPARENT_COLOR: u32 = 0x0000_0000;

/// A range of pixel values, `[start, end)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

impl Range {
    pub fn new(start: u32, end: u32) -> Self {
        Range { start, end }
    }
}

/// Inset lengths from all edges of a rectangle. `left`/`top` are measured
/// from the left/top edges, `right`/`bottom` from the right/bottom edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Bounds {
    pub fn new(left: u32, top: u32, right: u32, bottom: u32) -> Self {
        Bounds { left, top, right, bottom }
    }

    /// Port of `Bounds::nonZero`.
    pub fn non_zero(&self) -> bool {
        self.left != 0 || self.top != 0 || self.right != 0 || self.bottom != 0
    }
}

/// Nine-patch data extracted from a source image. All measurements
/// exclude the 1px border of the source 9-patch image.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NinePatch {
    /// Horizontal regions of the image that are stretchable.
    pub horizontal_stretch_regions: Vec<Range>,
    /// Vertical regions of the image that are stretchable.
    pub vertical_stretch_regions: Vec<Range>,
    /// 9-patch content padding/insets.
    pub padding: Bounds,
    /// Optical layout bounds/insets; overrides padding for layout purposes.
    pub layout_bounds: Bounds,
    /// The colors within each region, fixed or stretchable. For w*h
    /// regions, the color of region (x, y) is addressable via index
    /// `y * w + x`.
    pub region_colors: Vec<u32>,
    /// Outline of the image, calculated based on opacity.
    pub outline: Bounds,
    /// The computed radius of the outline. If non-zero, the outline is a
    /// rounded rect.
    pub outline_radius: f32,
    /// The largest alpha value within the outline.
    pub outline_alpha: u32,
}

/// Returns the alpha value encoded in the 0xAARRGGBB encoded pixel.
fn get_alpha(color: u32) -> u32 {
    (color & 0xff00_0000) >> 24
}

/// Packs an RGBA_8888 pixel as 0xAARRGGBB (the way 9-patch expects it).
/// Port of `NinePatch::PackRGBA`.
pub fn pack_rgba(pixel: [u8; 4]) -> u32 {
    ((pixel[3] as u32) << 24) | ((pixel[0] as u32) << 16) | ((pixel[1] as u32) << 8) | pixel[2] as u32
}

/// Determines whether a color on an image line is valid. A 9patch image
/// may use a transparent color as neutral, or a fully opaque white color
/// as neutral, based on the pixel color at (0,0) of the image. Port of
/// `ColorValidator` and its two subclasses.
#[derive(Clone, Copy)]
enum NeutralColor {
    Transparent,
    White,
}

impl NeutralColor {
    fn is_neutral(self, color: u32) -> bool {
        match self {
            NeutralColor::Transparent => get_alpha(color) == 0,
            NeutralColor::White => color == COLOR_OPAQUE_WHITE,
        }
    }

    fn is_valid(self, color: u32) -> bool {
        color == PRIMARY_COLOR || color == SECONDARY_COLOR || self.is_neutral(color)
    }
}

/// A row, column, or diagonal of pixels. Port of `HorizontalImageLine`,
/// `VerticalImageLine`, and `DiagonalImageLine`.
struct ImageLine<'a> {
    image: &'a Image,
    xoffset: i32,
    yoffset: i32,
    length: i32,
    axis: Axis,
}

enum Axis {
    Horizontal,
    Vertical,
    Diagonal { xstep: i32, ystep: i32 },
}

impl<'a> ImageLine<'a> {
    fn horizontal(image: &'a Image, xoffset: i32, yoffset: i32, length: i32) -> Self {
        ImageLine { image, xoffset, yoffset, length, axis: Axis::Horizontal }
    }

    fn vertical(image: &'a Image, xoffset: i32, yoffset: i32, length: i32) -> Self {
        ImageLine { image, xoffset, yoffset, length, axis: Axis::Vertical }
    }

    fn diagonal(image: &'a Image, xoffset: i32, yoffset: i32, xstep: i32, ystep: i32, length: i32) -> Self {
        ImageLine { image, xoffset, yoffset, length, axis: Axis::Diagonal { xstep, ystep } }
    }

    fn color(&self, idx: i32) -> u32 {
        let (x, y) = match self.axis {
            Axis::Horizontal => (idx + self.xoffset, self.yoffset),
            Axis::Vertical => (self.xoffset, self.yoffset + idx),
            Axis::Diagonal { xstep, ystep } => ((idx + self.xoffset) * xstep, self.yoffset + idx * ystep),
        };
        pack_rgba(self.image.pixel(x as usize, y as usize))
    }
}

/// Walks an image line and records ranges of primary (black: padding or
/// stretch) and secondary (red: optical bounds) colors. Port of
/// `FillRanges`. Range offsets are encoded without the 1px border.
fn fill_ranges(
    line: &ImageLine,
    validator: NeutralColor,
    primary_ranges: &mut Vec<Range>,
    secondary_ranges: &mut Vec<Range>,
) -> Result<(), String> {
    let length = line.length;
    let mut last_color: u32 = 0xffff_ffff;
    for idx in 1..length - 1 {
        let color = line.color(idx);
        if !validator.is_valid(color) {
            return Err("found an invalid color".to_string());
        }

        if color != last_color {
            // We are ending a range. Which range?
            if last_color == PRIMARY_COLOR {
                if let Some(range) = primary_ranges.last_mut() {
                    range.end = (idx - 1) as u32;
                }
            } else if last_color == SECONDARY_COLOR {
                if let Some(range) = secondary_ranges.last_mut() {
                    range.end = (idx - 1) as u32;
                }
            }

            // We are starting a range. Which range?
            if color == PRIMARY_COLOR {
                primary_ranges.push(Range::new((idx - 1) as u32, (length - 2) as u32));
            } else if color == SECONDARY_COLOR {
                secondary_ranges.push(Range::new((idx - 1) as u32, (length - 2) as u32));
            }
            last_color = color;
        }
    }
    Ok(())
}

/// Derives padding and layout-bounds insets from the ranges found on a
/// bottom/right border. Returns
/// `(padding_start, padding_end, layout_start, layout_end)`.
/// Port of `PopulateBounds`.
fn populate_bounds(
    padding: &[Range],
    layout_bounds: &[Range],
    stretch_regions: &[Range],
    length: u32,
    edge_name: &str,
) -> Result<(u32, u32, u32, u32), String> {
    if padding.len() > 1 {
        return Err(format!("too many padding sections on {edge_name} border"));
    }

    let mut padding_start = 0;
    let mut padding_end = 0;
    if let Some(range) = padding.first() {
        padding_start = range.start;
        padding_end = length.saturating_sub(range.end);
    } else if !stretch_regions.is_empty() {
        // No padding was defined. Compute the padding from the first and
        // last stretch regions.
        padding_start = stretch_regions[0].start;
        padding_end = length.saturating_sub(stretch_regions[stretch_regions.len() - 1].end);
    }

    if layout_bounds.len() > 2 {
        return Err(format!("too many layout bounds sections on {edge_name} border"));
    }

    let mut layout_start = 0;
    let mut layout_end = 0;
    if let Some(range) = layout_bounds.first() {
        // If there is only one layout bound segment, it might not start at
        // 0, but then it should end at length.
        if range.start != 0 && range.end != length {
            return Err(format!("layout bounds on {edge_name} border must start at edge"));
        }
        layout_start = range.end;

        if layout_bounds.len() >= 2 {
            let range = layout_bounds[layout_bounds.len() - 1];
            if range.end != length {
                return Err(format!("layout bounds on {edge_name} border must start at edge"));
            }
            layout_end = length.saturating_sub(range.start);
        }
    }
    Ok((padding_start, padding_end, layout_start, layout_end))
}

/// Port of `CalculateSegmentCount`.
fn calculate_segment_count(stretch_regions: &[Range], length: u32) -> i32 {
    if stretch_regions.is_empty() {
        return 0;
    }

    let start_is_fixed = stretch_regions[0].start != 0;
    let end_is_fixed = stretch_regions[stretch_regions.len() - 1].end != length;
    let modifier = if start_is_fixed && end_is_fixed {
        1
    } else if !start_is_fixed && !end_is_fixed {
        -1
    } else {
        0
    };
    stretch_regions.len() as i32 * 2 + modifier
}

/// Samples one region. If the whole region is the same color it is that
/// color; whole transparent regions get [`TRANSPARENT_COLOR`]; mixed
/// regions get [`NO_COLOR`]. Bounds are in bordered image coordinates.
/// Port of `GetRegionColor`.
fn get_region_color(image: &Image, region: Bounds) -> u32 {
    // Sample the first pixel to compare against.
    let expected_color = pack_rgba(image.pixel(region.left as usize, region.top as usize));
    for y in region.top..region.bottom {
        for x in region.left..region.right {
            let color = pack_rgba(image.pixel(x as usize, y as usize));
            if get_alpha(color) == 0 {
                // The color is transparent. If the expected color is not
                // transparent, NO_COLOR.
                if get_alpha(expected_color) != 0 {
                    return NO_COLOR;
                }
            } else if color != expected_color {
                return NO_COLOR;
            }
        }
    }

    if get_alpha(expected_color) == 0 {
        return TRANSPARENT_COLOR;
    }
    expected_color
}

/// Computes the color of every 9-patch section, in row-major order.
/// `width`/`height` are the dimensions WITHOUT the 1px border, and the
/// stretch-region indices exclude the border too, so all image accesses
/// are offset by 1. Port of `CalculateRegionColors`.
fn calculate_region_colors(
    image: &Image,
    horizontal_stretch_regions: &[Range],
    vertical_stretch_regions: &[Range],
    width: u32,
    height: u32,
) -> Vec<u32> {
    let mut colors = Vec::new();
    let mut next_top = 0u32;
    let mut row_iter = vertical_stretch_regions.iter().peekable();
    while next_top != height {
        let (top, bottom) = if let Some(region) = row_iter.peek() {
            if next_top != region.start {
                // This is a fixed segment.
                // Offset the bounds by 1 to accommodate the border.
                let bounds = (next_top + 1, region.start + 1);
                next_top = region.start;
                bounds
            } else {
                // This is a stretchy segment.
                let bounds = (region.start + 1, region.end + 1);
                next_top = region.end;
                row_iter.next();
                bounds
            }
        } else {
            // This is the end, fixed section.
            let bounds = (next_top + 1, height + 1);
            next_top = height;
            bounds
        };

        let mut next_left = 0u32;
        let mut col_iter = horizontal_stretch_regions.iter().peekable();
        while next_left != width {
            let (left, right) = if let Some(region) = col_iter.peek() {
                if next_left != region.start {
                    // This is a fixed segment.
                    let bounds = (next_left + 1, region.start + 1);
                    next_left = region.start;
                    bounds
                } else {
                    // This is a stretchy segment.
                    let bounds = (region.start + 1, region.end + 1);
                    next_left = region.end;
                    col_iter.next();
                    bounds
                }
            } else {
                // This is the end, fixed section.
                let bounds = (next_left + 1, width + 1);
                next_left = width;
                bounds
            };
            colors.push(get_region_color(image, Bounds::new(left, top, right, bottom)));
        }
    }
    colors
}

/// Calculates the insets of a row/column of pixels based on where the
/// largest alpha value begins (on both sides). Port of
/// `FindOutlineInsets`.
fn find_outline_insets(line: &ImageLine) -> (i32, i32) {
    let length = line.length;
    if length < 3 {
        return (0, 0);
    }

    // If the length is odd, we want both sides to process the center
    // pixel, so we use two different midpoints (to account for < and <=
    // in the different loops).
    let mid2 = length / 2;
    let mid1 = mid2 + (length % 2);

    let mut out_start = 0;
    let mut max_alpha = 0u32;
    let mut i = 0;
    while i < mid1 && max_alpha != 0xff {
        let alpha = get_alpha(line.color(i));
        if alpha > max_alpha {
            max_alpha = alpha;
            out_start = i;
        }
        i += 1;
    }

    let mut out_end = 0;
    max_alpha = 0;
    let mut i = length - 1;
    while i >= mid2 && max_alpha != 0xff {
        let alpha = get_alpha(line.color(i));
        if alpha > max_alpha {
            max_alpha = alpha;
            out_end = length - (i + 1);
        }
        i -= 1;
    }
    (out_start, out_end)
}

/// Port of `FindMaxAlpha`.
fn find_max_alpha(line: &ImageLine) -> u32 {
    let length = line.length;
    let mut max_alpha = 0u32;
    let mut idx = 0;
    while idx < length && max_alpha != 0xff {
        let alpha = get_alpha(line.color(idx));
        if alpha > max_alpha {
            max_alpha = alpha;
        }
        idx += 1;
    }
    max_alpha
}

/// Analyzes a full image INCLUDING its 1px nine-patch border. Port of
/// `NinePatch::Create`.
pub fn create(image: &Image) -> Result<NinePatch, String> {
    if image.width < 3 || image.height < 3 {
        return Err("image must be at least 3x3 (1x1 image with 1 pixel border)".to_string());
    }
    // Defensive checks not present in the C++ (which trusts raw row
    // pointers): make sure the pixel buffer matches the dimensions so the
    // accesses below cannot go out of bounds.
    if image.width > i32::MAX as usize || image.height > i32::MAX as usize {
        return Err("image dimensions are too large".to_string());
    }
    match image.width.checked_mul(image.height).and_then(|n| n.checked_mul(4)) {
        Some(expected) if image.pixels.len() >= expected => {}
        _ => return Err("image pixel buffer is smaller than the image dimensions".to_string()),
    }

    let width = image.width as i32;
    let height = image.height as i32;

    let corner = pack_rgba(image.pixel(0, 0));
    let validator = if get_alpha(corner) == 0 {
        NeutralColor::Transparent
    } else if corner == COLOR_OPAQUE_WHITE {
        NeutralColor::White
    } else {
        return Err("top-left corner pixel must be either opaque white or transparent".to_string());
    };

    let mut nine_patch = NinePatch::default();

    // Top border: horizontal stretch regions; red (layout bounds) is
    // unexpected here.
    let mut unexpected_ranges = Vec::new();
    let top_row = ImageLine::horizontal(image, 0, 0, width);
    fill_ranges(&top_row, validator, &mut nine_patch.horizontal_stretch_regions, &mut unexpected_ranges)?;
    if let Some(range) = unexpected_ranges.first() {
        return Err(format!(
            "found unexpected optical bounds (red pixel) on top border at x={}",
            range.start + 1
        ));
    }

    // Left border: vertical stretch regions.
    let left_col = ImageLine::vertical(image, 0, 0, height);
    fill_ranges(&left_col, validator, &mut nine_patch.vertical_stretch_regions, &mut unexpected_ranges)?;
    if let Some(range) = unexpected_ranges.first() {
        // NOTE: the C++ builds this message but (due to an upstream bug)
        // returns without assigning it to *out_err; we surface the
        // intended message.
        return Err(format!(
            "found unexpected optical bounds (red pixel) on left border at y={}",
            range.start + 1
        ));
    }

    // Bottom border: padding (black) and layout bounds (red).
    let mut horizontal_padding = Vec::new();
    let mut horizontal_layout_bounds = Vec::new();
    let bottom_row = ImageLine::horizontal(image, 0, height - 1, width);
    fill_ranges(&bottom_row, validator, &mut horizontal_padding, &mut horizontal_layout_bounds)?;
    let (pad_left, pad_right, layout_left, layout_right) = populate_bounds(
        &horizontal_padding,
        &horizontal_layout_bounds,
        &nine_patch.horizontal_stretch_regions,
        (width - 2) as u32,
        "bottom",
    )?;
    nine_patch.padding.left = pad_left;
    nine_patch.padding.right = pad_right;
    nine_patch.layout_bounds.left = layout_left;
    nine_patch.layout_bounds.right = layout_right;

    // Right border: padding (black) and layout bounds (red).
    let mut vertical_padding = Vec::new();
    let mut vertical_layout_bounds = Vec::new();
    let right_col = ImageLine::vertical(image, width - 1, 0, height);
    fill_ranges(&right_col, validator, &mut vertical_padding, &mut vertical_layout_bounds)?;
    let (pad_top, pad_bottom, layout_top, layout_bottom) = populate_bounds(
        &vertical_padding,
        &vertical_layout_bounds,
        &nine_patch.vertical_stretch_regions,
        (height - 2) as u32,
        "right",
    )?;
    nine_patch.padding.top = pad_top;
    nine_patch.padding.bottom = pad_bottom;
    nine_patch.layout_bounds.top = layout_top;
    nine_patch.layout_bounds.bottom = layout_bottom;

    // Fill the region colors of the 9-patch.
    let num_rows = calculate_segment_count(&nine_patch.horizontal_stretch_regions, (width - 2) as u32);
    let num_cols = calculate_segment_count(&nine_patch.vertical_stretch_regions, (height - 2) as u32);
    if num_rows as i64 * num_cols as i64 > 0x7f {
        return Err("too many regions in 9-patch".to_string());
    }

    nine_patch.region_colors = calculate_region_colors(
        image,
        &nine_patch.horizontal_stretch_regions,
        &nine_patch.vertical_stretch_regions,
        (width - 2) as u32,
        (height - 2) as u32,
    );

    // Compute the outline based on opacity.

    // Find left and right extent of 9-patch content on center row.
    let mid_row = ImageLine::horizontal(image, 1, height / 2, width - 2);
    let (outline_left, outline_right) = find_outline_insets(&mid_row);
    nine_patch.outline.left = outline_left as u32;
    nine_patch.outline.right = outline_right as u32;

    // Find top and bottom extent of 9-patch content on center column.
    let mid_col = ImageLine::vertical(image, width / 2, 1, height - 2);
    let (outline_top, outline_bottom) = find_outline_insets(&mid_col);
    nine_patch.outline.top = outline_top as u32;
    nine_patch.outline.bottom = outline_bottom as u32;

    let outline_width = (width - 2) - outline_left - outline_right;
    let outline_height = (height - 2) - outline_top - outline_bottom;

    // Find the largest alpha value within the outline area.
    let outline_mid_row = ImageLine::horizontal(
        image,
        1 + outline_left,
        1 + outline_top + outline_height / 2,
        outline_width,
    );
    let outline_mid_col = ImageLine::vertical(
        image,
        1 + outline_left + outline_width / 2,
        1 + outline_top,
        outline_height,
    );
    nine_patch.outline_alpha = find_max_alpha(&outline_mid_row).max(find_max_alpha(&outline_mid_col));

    // Assuming the image is a round rect, compute the radius by marching
    // diagonally from the top left corner towards the center.
    let diagonal = ImageLine::diagonal(
        image,
        1 + outline_left,
        1 + outline_top,
        1,
        1,
        outline_width.min(outline_height),
    );
    let (top_left, _bottom_right) = find_outline_insets(&diagonal);

    // Determine source radius based upon inset:
    //     sqrt(r^2 + r^2) = sqrt(i^2 + i^2) + r
    //     sqrt(2) * r = sqrt(2) * i + r
    //     (sqrt(2) - 1) * r = sqrt(2) * i
    //     r = sqrt(2) / (sqrt(2) - 1) * i
    nine_patch.outline_radius = 3.4142_f32 * top_left as f32;

    Ok(nine_patch)
}

fn write_divs_be(out: &mut Vec<u8>, regions: &[Range], count: u8) {
    for value in regions.iter().flat_map(|r| [r.start, r.end]).take(count as usize) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl NinePatch {
    /// `npTc` chunk payload: `Res_png_9patch` in file order. Port of
    /// `NinePatch::SerializeBase` (which is `Res_png_9patch::serialize` +
    /// `fill9patchOffsets` + `deviceToFile`).
    ///
    /// Layout (packed, 32-byte header followed by the arrays):
    /// - `wasDeserialized` u8 = 0, `numXDivs` u8, `numYDivs` u8,
    ///   `numColors` u8
    /// - `xDivsOffset` u32, `yDivsOffset` u32 — LITTLE-endian (host
    ///   order; `deviceToFile` does not convert these)
    /// - `paddingLeft/Right/Top/Bottom` u32 each — BIG-endian (`htonl`)
    /// - `colorsOffset` u32 — LITTLE-endian
    /// - `xDivs`, `yDivs` (start/end pairs), `colors` — BIG-endian
    pub fn serialize_tc(&self) -> Vec<u8> {
        // The casts mirror `static_cast<uint8_t>(size()) * 2` in
        // SerializeBase.
        let num_x_divs = (self.horizontal_stretch_regions.len() as u8).wrapping_mul(2);
        let num_y_divs = (self.vertical_stretch_regions.len() as u8).wrapping_mul(2);
        let num_colors = self.region_colors.len() as u8;

        // fill9patchOffsets: offsets are from the start of the struct.
        let x_divs_offset = 32u32;
        let y_divs_offset = x_divs_offset + num_x_divs as u32 * 4;
        let colors_offset = y_divs_offset + num_y_divs as u32 * 4;

        let serialized_size =
            32 + num_x_divs as usize * 4 + num_y_divs as usize * 4 + num_colors as usize * 4;
        let mut out = Vec::with_capacity(serialized_size);
        out.push(0); // wasDeserialized
        out.push(num_x_divs);
        out.push(num_y_divs);
        out.push(num_colors);
        out.extend_from_slice(&x_divs_offset.to_le_bytes());
        out.extend_from_slice(&y_divs_offset.to_le_bytes());
        out.extend_from_slice(&self.padding.left.to_be_bytes());
        out.extend_from_slice(&self.padding.right.to_be_bytes());
        out.extend_from_slice(&self.padding.top.to_be_bytes());
        out.extend_from_slice(&self.padding.bottom.to_be_bytes());
        out.extend_from_slice(&colors_offset.to_le_bytes());

        write_divs_be(&mut out, &self.horizontal_stretch_regions, num_x_divs);
        write_divs_be(&mut out, &self.vertical_stretch_regions, num_y_divs);
        for color in self.region_colors.iter().take(num_colors as usize) {
            out.extend_from_slice(&color.to_be_bytes());
        }

        // serializedSize() is computed from the (possibly wrapped) u8
        // counts; keep the buffer length consistent with the header.
        out.resize(serialized_size, 0);
        out
    }

    /// `npOl` chunk payload: 6 host-order (little-endian) u32-sized
    /// values: outline left/top/right/bottom, outline radius (f32 bits),
    /// outline alpha. Port of `NinePatch::SerializeRoundedRectOutline`.
    pub fn serialize_outline(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(&self.outline.left.to_le_bytes());
        out.extend_from_slice(&self.outline.top.to_le_bytes());
        out.extend_from_slice(&self.outline.right.to_le_bytes());
        out.extend_from_slice(&self.outline.bottom.to_le_bytes());
        out.extend_from_slice(&self.outline_radius.to_le_bytes());
        out.extend_from_slice(&self.outline_alpha.to_le_bytes());
        out
    }

    /// `npLb` chunk payload: 4 host-order (little-endian) u32 values:
    /// layout bounds left/top/right/bottom. Returns `None` when there are
    /// no layout bounds, matching `WriteNinePatch`'s
    /// `layout_bounds.nonZero()` gate. Port of
    /// `NinePatch::SerializeLayoutBounds`.
    pub fn serialize_layout_bounds(&self) -> Option<Vec<u8>> {
        if !self.layout_bounds.non_zero() {
            return None;
        }
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(&self.layout_bounds.left.to_le_bytes());
        out.extend_from_slice(&self.layout_bounds.top.to_le_bytes());
        out.extend_from_slice(&self.layout_bounds.right.to_le_bytes());
        out.extend_from_slice(&self.layout_bounds.bottom.to_le_bytes());
        Some(out)
    }
}

// Tests ported from libs/androidfw/tests/NinePatch_test.cpp.
#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an RGBA test image from a character map, mirroring the
    /// pixel macros of the C++ test:
    /// `W`=WHITE `B`=BLACK `R`=RED `b`=BLUE `T`=TRANS,
    /// `G`=GREEN (NOTE: the upstream fixture defines GREEN with the same
    /// bytes as RED — preserved here), `7`/`5`/`2` = GR_70/GR_50/GR_20.
    fn px(c: char) -> [u8; 4] {
        match c {
            'W' => [0xff, 0xff, 0xff, 0xff],
            'B' => [0x00, 0x00, 0x00, 0xff],
            'R' => [0xff, 0x00, 0x00, 0xff],
            'b' => [0x00, 0x00, 0xff, 0xff],
            'G' => [0xff, 0x00, 0x00, 0xff], // sic, see above
            'T' => [0x00, 0x00, 0x00, 0x00],
            '7' => [0xff, 0x00, 0x00, 0xb3],
            '5' => [0xff, 0x00, 0x00, 0x80],
            '2' => [0xff, 0x00, 0x00, 0x33],
            other => panic!("unknown test pixel '{other}'"),
        }
    }

    fn img(rows: &[&str]) -> Image {
        let height = rows.len();
        let width = rows[0].chars().count();
        let mut pixels = Vec::with_capacity(width * height * 4);
        for row in rows {
            assert_eq!(row.chars().count(), width, "ragged test image");
            for c in row.chars() {
                pixels.extend_from_slice(&px(c));
            }
        }
        Image { width, height, pixels }
    }

    fn stretch_and_padding_5x5() -> Image {
        img(&[
            "WWBWW", //
            "WRRRW", //
            "BRRRB", //
            "WRRRW", //
            "WWBWW",
        ])
    }

    #[test]
    fn minimum_3x3() {
        let err = create(&img(&["WW", "WW"])).unwrap_err();
        assert_eq!(err, "image must be at least 3x3 (1x1 image with 1 pixel border)");
    }

    #[test]
    fn mixed_neutral_colors() {
        // White corner selects the white-neutral validator; the
        // transparent pixel on the left border is then invalid.
        let err = create(&img(&["WBT", "TRT", "WWW"])).unwrap_err();
        assert_eq!(err, "found an invalid color");
    }

    #[test]
    fn transparent_neutral_color() {
        assert!(create(&img(&["TBT", "BRB", "TBT"])).is_ok());
    }

    #[test]
    fn single_stretch_region() {
        let nine_patch = create(&img(&[
            "WWBBBWW", //
            "WRRRRRW", //
            "BRRRRRW", //
            "BRRRRRW", //
            "WRRRRRW", //
            "WWWWWWW",
        ]))
        .unwrap();

        assert_eq!(nine_patch.horizontal_stretch_regions, vec![Range::new(1, 4)]);
        assert_eq!(nine_patch.vertical_stretch_regions, vec![Range::new(1, 3)]);
    }

    fn multiple_stretch_10x7() -> Image {
        img(&[
            "WWBWBBWBWW", //
            "BRbRbbRbRW", //
            "BRbRbbRbRW", //
            "WRbRbbRbRW", //
            "BRbRbbRbRW", //
            "BRbRbbRbRW", //
            "WWWWWWWWWW",
        ])
    }

    #[test]
    fn multiple_stretch_regions() {
        let nine_patch = create(&multiple_stretch_10x7()).unwrap();

        assert_eq!(
            nine_patch.horizontal_stretch_regions,
            vec![Range::new(1, 2), Range::new(3, 5), Range::new(6, 7)]
        );
        assert_eq!(
            nine_patch.vertical_stretch_regions,
            vec![Range::new(0, 2), Range::new(3, 5)]
        );
    }

    #[test]
    fn infer_padding_from_stretch_regions() {
        let nine_patch = create(&multiple_stretch_10x7()).unwrap();
        assert_eq!(nine_patch.padding, Bounds::new(1, 0, 1, 0));
    }

    #[test]
    fn padding() {
        let nine_patch = create(&img(&[
            "WWWWWW", //
            "WWWWWW", //
            "WWWWWB", //
            "WWWWWW", //
            "WWBBWW",
        ]))
        .unwrap();
        assert_eq!(nine_patch.padding, Bounds::new(1, 1, 1, 1));
    }

    #[test]
    fn layout_bounds_are_on_wrong_edge() {
        let err = create(&img(&["WRW", "RWW", "WWW"])).unwrap_err();
        assert_eq!(err, "found unexpected optical bounds (red pixel) on top border at x=1");
    }

    #[test]
    fn layout_bounds_must_touch_edges() {
        let err = create(&img(&[
            "WWWWW", //
            "WWWWW", //
            "WWWWR", //
            "WWWWW", //
            "WWRWW",
        ]))
        .unwrap_err();
        assert_eq!(err, "layout bounds on bottom border must start at edge");
    }

    fn layout_bounds_5x5() -> Image {
        img(&[
            "WWWWW", //
            "WWWWR", //
            "WWWWW", //
            "WWWWR", //
            "WRWRW",
        ])
    }

    #[test]
    fn layout_bounds() {
        let nine_patch = create(&layout_bounds_5x5()).unwrap();
        assert_eq!(nine_patch.layout_bounds, Bounds::new(1, 1, 1, 1));

        let nine_patch = create(&img(&[
            "WWWWW", //
            "WWWWR", //
            "WWWWW", //
            "WWWWW", //
            "WRWWW",
        ]))
        .unwrap();
        assert_eq!(nine_patch.layout_bounds, Bounds::new(1, 1, 0, 0));
    }

    #[test]
    fn padding_and_layout_bounds() {
        let nine_patch = create(&img(&[
            "WWWWW", //
            "WWWWR", //
            "WWWWB", //
            "WWWWR", //
            "WRBRW",
        ]))
        .unwrap();
        assert_eq!(nine_patch.padding, Bounds::new(1, 1, 1, 1));
        assert_eq!(nine_patch.layout_bounds, Bounds::new(1, 1, 1, 1));
    }

    #[test]
    fn region_colors_are_correct() {
        let nine_patch = create(&img(&[
            "WBWBW", //
            "BRbGW", //
            "BRGGW", //
            "WTbGW", //
            "WWWWW",
        ]))
        .unwrap();

        let expected_colors = vec![
            0xffff_0000u32,    // red
            NO_COLOR,          // blue/"green" mix
            0xffff_0000,       // "green" (same bytes as red in the fixture)
            TRANSPARENT_COLOR, //
            0xff00_00ff,       // blue
            0xffff_0000,       // "green"
        ];
        assert_eq!(nine_patch.region_colors, expected_colors);
    }

    fn outline_opaque_10x10() -> Image {
        img(&[
            "WBBBBBBBBW", //
            "WTTTTTTTTW", //
            "WTTTTTTTTW", //
            "WTTGGGGTTW", //
            "WTTGGGGTTW", //
            "WTTGGGGTTW", //
            "WTTGGGGTTW", //
            "WTTTTTTTTW", //
            "WTTTTTTTTW", //
            "WWWWWWWWWW",
        ])
    }

    #[test]
    fn outline_from_opaque_image() {
        let nine_patch = create(&outline_opaque_10x10()).unwrap();
        assert_eq!(nine_patch.outline, Bounds::new(2, 2, 2, 2));
        assert_eq!(nine_patch.outline_alpha, 0x0000_00ff);
        assert_eq!(nine_patch.outline_radius, 0.0);
    }

    #[test]
    fn outline_from_translucent_image() {
        let nine_patch = create(&img(&[
            "WBBBBBBBBW", //
            "WTTTTTTTTW", //
            "WTT2222TTW", //
            "WTT5555TTW", //
            "WT257752TW", //
            "WT257752TW", //
            "WTT5555TTW", //
            "WTT2222TTW", //
            "WTTTTTTTTW", //
            "WWWWWWWWWW",
        ]))
        .unwrap();
        assert_eq!(nine_patch.outline, Bounds::new(3, 3, 3, 3));
        assert_eq!(nine_patch.outline_alpha, 0x0000_00b3);
        assert_eq!(nine_patch.outline_radius, 0.0);
    }

    #[test]
    fn outline_from_off_center_image() {
        let nine_patch = create(&img(&[
            "WWWBBBBBBBBW", //
            "WTTTTTTTTTTW", //
            "WTTTT2222TTW", //
            "WTTTT5555TTW", //
            "WTTT257752TW", //
            "WTTT257752TW", //
            "WTTTT5555TTW", //
            "WTTTT2222TTW", //
            "WTTTTTTTTTTW", //
            "WWWWWWWWWWWW",
        ]))
        .unwrap();

        // The (preserved) C++ algorithm searches from the outside to the
        // middle for each inset; with a shifted outline the search may
        // not find the closer bound, hence (4, ...) and not (5, ...).
        assert_eq!(nine_patch.outline, Bounds::new(4, 3, 3, 3));
        assert_eq!(nine_patch.outline_alpha, 0x0000_00b3);
        assert_eq!(nine_patch.outline_radius, 0.0);
    }

    #[test]
    fn outline_radius() {
        let nine_patch = create(&img(&[
            "WBBBW", //
            "BTGTW", //
            "BGGGW", //
            "BTGTW", //
            "WWWWW",
        ]))
        .unwrap();
        assert_eq!(nine_patch.outline, Bounds::new(0, 0, 0, 0));
        assert_eq!(nine_patch.outline_radius, 3.4142);
    }

    #[test]
    fn serialize_png_endianness() {
        let nine_patch = create(&stretch_and_padding_5x5()).unwrap();
        let data = nine_patch.serialize_tc();
        assert!(!data.is_empty());

        // Skip past wasDeserialized + numXDivs + numYDivs + numColors +
        // xDivsOffset + yDivsOffset (12 bytes).
        // Check that padding is big-endian, expecting value 1.
        for offset in [12usize, 16, 20, 24] {
            assert_eq!(
                &data[offset..offset + 4],
                &[0x00, 0x00, 0x00, 0x01],
                "padding at byte offset {offset} is not big-endian 1"
            );
        }
    }

    #[test]
    fn serialize_tc_exact_bytes() {
        // 1 horizontal + 1 vertical stretch region, padding of 1 on all
        // edges, and 3x3 = 9 regions that are all solid red.
        let nine_patch = create(&stretch_and_padding_5x5()).unwrap();
        assert_eq!(nine_patch.horizontal_stretch_regions, vec![Range::new(1, 2)]);
        assert_eq!(nine_patch.vertical_stretch_regions, vec![Range::new(1, 2)]);
        assert_eq!(nine_patch.padding, Bounds::new(1, 1, 1, 1));
        assert_eq!(nine_patch.region_colors.len(), 9);

        let mut expected = vec![
            0x00, // wasDeserialized
            0x02, // numXDivs
            0x02, // numYDivs
            0x09, // numColors
            0x20, 0x00, 0x00, 0x00, // xDivsOffset = 32 (little-endian)
            0x28, 0x00, 0x00, 0x00, // yDivsOffset = 40 (little-endian)
            0x00, 0x00, 0x00, 0x01, // paddingLeft = 1 (big-endian)
            0x00, 0x00, 0x00, 0x01, // paddingRight = 1 (big-endian)
            0x00, 0x00, 0x00, 0x01, // paddingTop = 1 (big-endian)
            0x00, 0x00, 0x00, 0x01, // paddingBottom = 1 (big-endian)
            0x30, 0x00, 0x00, 0x00, // colorsOffset = 48 (little-endian)
            0x00, 0x00, 0x00, 0x01, // xDivs[0] = 1 (big-endian)
            0x00, 0x00, 0x00, 0x02, // xDivs[1] = 2 (big-endian)
            0x00, 0x00, 0x00, 0x01, // yDivs[0] = 1 (big-endian)
            0x00, 0x00, 0x00, 0x02, // yDivs[1] = 2 (big-endian)
        ];
        for _ in 0..9 {
            // 0xffff0000 (opaque red) big-endian.
            expected.extend_from_slice(&[0xff, 0xff, 0x00, 0x00]);
        }
        assert_eq!(expected.len(), 84);
        assert_eq!(nine_patch.serialize_tc(), expected);
    }

    #[test]
    fn serialize_outline_bytes() {
        let nine_patch = create(&outline_opaque_10x10()).unwrap();
        assert_eq!(
            nine_patch.serialize_outline(),
            vec![
                0x02, 0x00, 0x00, 0x00, // left (little-endian)
                0x02, 0x00, 0x00, 0x00, // top
                0x02, 0x00, 0x00, 0x00, // right
                0x02, 0x00, 0x00, 0x00, // bottom
                0x00, 0x00, 0x00, 0x00, // radius = 0.0f
                0xff, 0x00, 0x00, 0x00, // alpha = 0xff (little-endian)
            ]
        );
    }

    #[test]
    fn serialize_layout_bounds_bytes() {
        // No layout bounds: no npLb chunk.
        let nine_patch = create(&stretch_and_padding_5x5()).unwrap();
        assert_eq!(nine_patch.serialize_layout_bounds(), None);

        let nine_patch = create(&layout_bounds_5x5()).unwrap();
        assert_eq!(
            nine_patch.serialize_layout_bounds(),
            Some(vec![
                0x01, 0x00, 0x00, 0x00, // left (little-endian)
                0x01, 0x00, 0x00, 0x00, // top
                0x01, 0x00, 0x00, 0x00, // right
                0x01, 0x00, 0x00, 0x00, // bottom
            ])
        );
    }
}
