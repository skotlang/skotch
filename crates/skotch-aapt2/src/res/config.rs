//! Resource configuration (`ResTable_config`): qualifier-string parsing,
//! printing, comparison/matching, and the 64-byte binary form used in
//! `resources.arsc`. Port of androidfw ConfigDescription/ResTable_config.
//!
//! Sources ported:
//! - `libs/androidfw/ConfigDescription.cpp` (qualifier parsing,
//!   `ApplyVersionForCompatibility`, `Dominates`/`HasHigherPrecedenceThan`/
//!   `ConflictsWith`/`IsCompatibleWith`)
//! - `libs/androidfw/ResourceTypes.cpp` (`ResTable_config::compare`,
//!   `compareLogical`, `diff`, `isMoreSpecificThan`, `match`, `isBetterThan`,
//!   `isLocaleMoreSpecificThan`, `isLocaleBetterThan`, `toString`,
//!   `appendDirLocale`, `getBcp47Locale`, `setBcp47Locale`,
//!   pack/unpackLanguageOrRegion)
//! - `libs/androidfw/Locale.cpp` (`LocaleValue` and `InitFromParts`)
//!
//! Deliberate deviations from the C++:
//! - The leading `size` field of `ResTable_config` is not stored as a struct
//!   field. `ConfigDescription` always keeps it at `sizeof(ResTable_config)`
//!   (64), so it carries no information; [`ConfigDescription::to_bytes`]
//!   writes [`ConfigDescription::SIZE`] and [`ConfigDescription::from_bytes`]
//!   honors a smaller/larger declared size exactly like `copyFromDtoH`.
//! - The CLDR-generated locale data tables (`LocaleDataTables.cpp`) are not
//!   embedded. `locale_data_compute_script` therefore never finds a likely
//!   script (`match` falls back to its countries-must-match path, which is
//!   also what happens in C++ for locales missing from the table), the
//!   locale parent tree is just `lang-REGION -> lang -> root`, and no locale
//!   is "representative". This only affects fine-grained locale tie-breaking
//!   in `is_locale_better_than`/`match` for configs with script data.
//! - `localeDataCompareRegions`'s final tie-break returns the *sign* of
//!   `right - left` (the C++ truncates an `int64_t` difference to `int`;
//!   callers only inspect the sign).

use std::cmp::Ordering;
use std::fmt;

// ---------------------------------------------------------------------------
// SDK version codes (androidfw/ConfigDescription.h `ApiVersion` enum)
// ---------------------------------------------------------------------------

pub const SDK_CUPCAKE: u16 = 3;
pub const SDK_DONUT: u16 = 4;
pub const SDK_ECLAIR: u16 = 5;
pub const SDK_ECLAIR_0_1: u16 = 6;
pub const SDK_ECLAIR_MR1: u16 = 7;
pub const SDK_FROYO: u16 = 8;
pub const SDK_GINGERBREAD: u16 = 9;
pub const SDK_GINGERBREAD_MR1: u16 = 10;
pub const SDK_HONEYCOMB: u16 = 11;
pub const SDK_HONEYCOMB_MR1: u16 = 12;
pub const SDK_HONEYCOMB_MR2: u16 = 13;
pub const SDK_ICE_CREAM_SANDWICH: u16 = 14;
pub const SDK_ICE_CREAM_SANDWICH_MR1: u16 = 15;
pub const SDK_JELLY_BEAN: u16 = 16;
pub const SDK_JELLY_BEAN_MR1: u16 = 17;
pub const SDK_JELLY_BEAN_MR2: u16 = 18;
pub const SDK_KITKAT: u16 = 19;
pub const SDK_KITKAT_WATCH: u16 = 20;
pub const SDK_LOLLIPOP: u16 = 21;
pub const SDK_LOLLIPOP_MR1: u16 = 22;
pub const SDK_MARSHMALLOW: u16 = 23;
pub const SDK_NOUGAT: u16 = 24;
pub const SDK_NOUGAT_MR1: u16 = 25;
pub const SDK_O: u16 = 26;
pub const SDK_O_MR1: u16 = 27;
pub const SDK_P: u16 = 28;
pub const SDK_Q: u16 = 29;
pub const SDK_R: u16 = 30;
pub const SDK_S: u16 = 31;
pub const SDK_S_V2: u16 = 32;
pub const SDK_TIRAMISU: u16 = 33;
pub const SDK_U: u16 = 34;

/// `ACONFIGURATION_MNC_ZERO`: the value stored for an explicit `mnc00`.
pub const MNC_ZERO: u16 = 0xffff;

// ---------------------------------------------------------------------------
// ConfigDescription
// ---------------------------------------------------------------------------

/// A resource configuration. Field-for-field mirror of `ResTable_config`
/// (minus the redundant leading `size` field — see module docs).
///
/// Note on equality: `PartialEq`/`Eq`/`Hash` are derived and compare every
/// field byte-for-byte. The C++ `ConfigDescription::operator==` is
/// `compare() == 0`, which masks `localeScript` when it was computed and
/// ignores `screenConfigPad2`; for configs produced by [`Self::parse`] the
/// two notions coincide. The ported relational methods (`dominates`,
/// `has_higher_precedence_than`) use [`Self::compare`] internally, exactly
/// like the C++.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ConfigDescription {
    /// Mobile country code (from SIM). 0 means "any".
    pub mcc: u16,
    /// Mobile network code (from SIM). 0 means "any", [`MNC_ZERO`] means "00".
    pub mnc: u16,
    /// Two 7-bit ASCII chars or a packed 3-letter ISO-639-2 code
    /// (high bit of byte 0 set). `[0, 0]` means "any".
    pub language: [u8; 2],
    /// Two 7-bit ASCII chars or a packed UN M.49 3-digit region code.
    pub country: [u8; 2],
    pub orientation: u8,
    pub touchscreen: u8,
    pub density: u16,
    pub keyboard: u8,
    pub navigation: u8,
    pub input_flags: u8,
    /// `grammaticalInflection`: occupies the byte the C++ union calls
    /// `inputFieldPad0`.
    pub grammatical_inflection: u8,
    pub screen_width: u16,
    pub screen_height: u16,
    pub sdk_version: u16,
    /// Must always be 0; its meaning is currently undefined.
    pub minor_version: u16,
    pub screen_layout: u8,
    pub ui_mode: u8,
    pub smallest_screen_width_dp: u16,
    pub screen_width_dp: u16,
    pub screen_height_dp: u16,
    /// ISO-15924 short script name (e.g. `Latn`), zero-filled.
    pub locale_script: [u8; 4],
    /// A single BCP-47 variant subtag (4–8 chars), zero-filled.
    pub locale_variant: [u8; 8],
    /// Contains the round/notround qualifier.
    pub screen_layout2: u8,
    /// Wide-gamut, HDR, etc.
    pub color_mode: u8,
    /// Reserved padding (`screenConfigPad2`). Always written/read so binary
    /// round-trips are exact.
    pub screen_config_pad2: u16,
    /// If false and `locale_script` is set, the script was explicitly
    /// provided. If true, `locale_script` was (or could not be) computed.
    pub locale_script_was_computed: bool,
    /// BCP-47 Unicode extension for key `nu` (numbering system), 3–8 chars,
    /// zero-filled.
    pub locale_numbering_system: [u8; 8],
}

impl ConfigDescription {
    /// `sizeof(ResTable_config)`: the canonical full size aapt2 writes.
    ///
    /// Layout (all little-endian):
    /// `size:u32 @0, mcc:u16 @4, mnc:u16 @6, language[2] @8, country[2] @10,
    /// orientation:u8 @12, touchscreen:u8 @13, density:u16 @14,
    /// keyboard:u8 @16, navigation:u8 @17, inputFlags:u8 @18,
    /// grammaticalInflection:u8 @19, screenWidth:u16 @20, screenHeight:u16 @22,
    /// sdkVersion:u16 @24, minorVersion:u16 @26, screenLayout:u8 @28,
    /// uiMode:u8 @29, smallestScreenWidthDp:u16 @30, screenWidthDp:u16 @32,
    /// screenHeightDp:u16 @34, localeScript[4] @36, localeVariant[8] @40,
    /// screenLayout2:u8 @48, colorMode:u8 @49, screenConfigPad2:u16 @50,
    /// localeScriptWasComputed:u8 @52, localeNumberingSystem[8] @53,
    /// padding[3] @61` — total 64 bytes.
    pub const SIZE: usize = 64;

    // --- orientation ---
    pub const ORIENTATION_ANY: u8 = 0;
    pub const ORIENTATION_PORT: u8 = 1;
    pub const ORIENTATION_LAND: u8 = 2;
    pub const ORIENTATION_SQUARE: u8 = 3;

    // --- touchscreen ---
    pub const TOUCHSCREEN_ANY: u8 = 0;
    pub const TOUCHSCREEN_NOTOUCH: u8 = 1;
    pub const TOUCHSCREEN_STYLUS: u8 = 2;
    pub const TOUCHSCREEN_FINGER: u8 = 3;

    // --- density ---
    pub const DENSITY_DEFAULT: u16 = 0;
    pub const DENSITY_LOW: u16 = 120;
    pub const DENSITY_MEDIUM: u16 = 160;
    pub const DENSITY_TV: u16 = 213;
    pub const DENSITY_HIGH: u16 = 240;
    pub const DENSITY_XHIGH: u16 = 320;
    pub const DENSITY_XXHIGH: u16 = 480;
    pub const DENSITY_XXXHIGH: u16 = 640;
    pub const DENSITY_ANY: u16 = 0xfffe;
    pub const DENSITY_NONE: u16 = 0xffff;

    // --- keyboard ---
    pub const KEYBOARD_ANY: u8 = 0;
    pub const KEYBOARD_NOKEYS: u8 = 1;
    pub const KEYBOARD_QWERTY: u8 = 2;
    pub const KEYBOARD_12KEY: u8 = 3;

    // --- navigation ---
    pub const NAVIGATION_ANY: u8 = 0;
    pub const NAVIGATION_NONAV: u8 = 1;
    pub const NAVIGATION_DPAD: u8 = 2;
    pub const NAVIGATION_TRACKBALL: u8 = 3;
    pub const NAVIGATION_WHEEL: u8 = 4;

    // --- inputFlags ---
    pub const MASK_KEYSHIDDEN: u8 = 0x0003;
    pub const KEYSHIDDEN_ANY: u8 = 0;
    pub const KEYSHIDDEN_NO: u8 = 1;
    pub const KEYSHIDDEN_YES: u8 = 2;
    pub const KEYSHIDDEN_SOFT: u8 = 3;

    pub const MASK_NAVHIDDEN: u8 = 0x000c;
    pub const SHIFT_NAVHIDDEN: u8 = 2;
    pub const NAVHIDDEN_ANY: u8 = 0;
    pub const NAVHIDDEN_NO: u8 = 1 << Self::SHIFT_NAVHIDDEN;
    pub const NAVHIDDEN_YES: u8 = 2 << Self::SHIFT_NAVHIDDEN;

    // --- grammatical inflection ---
    pub const GRAMMATICAL_GENDER_ANY: u8 = 0;
    pub const GRAMMATICAL_GENDER_NEUTER: u8 = 1;
    pub const GRAMMATICAL_GENDER_FEMININE: u8 = 2;
    pub const GRAMMATICAL_GENDER_MASCULINE: u8 = 3;
    pub const GRAMMATICAL_INFLECTION_GENDER_MASK: u8 = 0b11;

    // --- screen size / version "any" ---
    pub const SCREENWIDTH_ANY: u16 = 0;
    pub const SCREENHEIGHT_ANY: u16 = 0;
    pub const SDKVERSION_ANY: u16 = 0;
    pub const MINORVERSION_ANY: u16 = 0;

    // --- screenLayout ---
    pub const MASK_SCREENSIZE: u8 = 0x0f;
    pub const SCREENSIZE_ANY: u8 = 0;
    pub const SCREENSIZE_SMALL: u8 = 1;
    pub const SCREENSIZE_NORMAL: u8 = 2;
    pub const SCREENSIZE_LARGE: u8 = 3;
    pub const SCREENSIZE_XLARGE: u8 = 4;

    pub const MASK_SCREENLONG: u8 = 0x30;
    pub const SHIFT_SCREENLONG: u8 = 4;
    pub const SCREENLONG_ANY: u8 = 0;
    pub const SCREENLONG_NO: u8 = 1 << Self::SHIFT_SCREENLONG;
    pub const SCREENLONG_YES: u8 = 2 << Self::SHIFT_SCREENLONG;

    pub const MASK_LAYOUTDIR: u8 = 0xC0;
    pub const SHIFT_LAYOUTDIR: u8 = 6;
    pub const LAYOUTDIR_ANY: u8 = 0;
    pub const LAYOUTDIR_LTR: u8 = 1 << Self::SHIFT_LAYOUTDIR;
    pub const LAYOUTDIR_RTL: u8 = 2 << Self::SHIFT_LAYOUTDIR;

    // --- uiMode ---
    pub const MASK_UI_MODE_TYPE: u8 = 0x0f;
    pub const UI_MODE_TYPE_ANY: u8 = 0;
    pub const UI_MODE_TYPE_NORMAL: u8 = 1;
    pub const UI_MODE_TYPE_DESK: u8 = 2;
    pub const UI_MODE_TYPE_CAR: u8 = 3;
    pub const UI_MODE_TYPE_TELEVISION: u8 = 4;
    pub const UI_MODE_TYPE_APPLIANCE: u8 = 5;
    pub const UI_MODE_TYPE_WATCH: u8 = 6;
    pub const UI_MODE_TYPE_VR_HEADSET: u8 = 7;

    pub const MASK_UI_MODE_NIGHT: u8 = 0x30;
    pub const SHIFT_UI_MODE_NIGHT: u8 = 4;
    pub const UI_MODE_NIGHT_ANY: u8 = 0;
    pub const UI_MODE_NIGHT_NO: u8 = 1 << Self::SHIFT_UI_MODE_NIGHT;
    pub const UI_MODE_NIGHT_YES: u8 = 2 << Self::SHIFT_UI_MODE_NIGHT;

    // --- screenLayout2 ---
    pub const MASK_SCREENROUND: u8 = 0x03;
    pub const SCREENROUND_ANY: u8 = 0;
    pub const SCREENROUND_NO: u8 = 1;
    pub const SCREENROUND_YES: u8 = 2;

    // --- colorMode ---
    pub const MASK_WIDE_COLOR_GAMUT: u8 = 0x03;
    pub const WIDE_COLOR_GAMUT_ANY: u8 = 0;
    pub const WIDE_COLOR_GAMUT_NO: u8 = 1;
    pub const WIDE_COLOR_GAMUT_YES: u8 = 2;

    pub const MASK_HDR: u8 = 0x0c;
    pub const SHIFT_COLOR_MODE_HDR: u8 = 2;
    pub const HDR_ANY: u8 = 0;
    pub const HDR_NO: u8 = 1 << Self::SHIFT_COLOR_MODE_HDR;
    pub const HDR_YES: u8 = 2 << Self::SHIFT_COLOR_MODE_HDR;

    // --- CONFIG_* diff bits (match ACONFIGURATION_* / ActivityInfo) ---
    pub const CONFIG_MCC: u32 = 0x0001;
    pub const CONFIG_MNC: u32 = 0x0002;
    pub const CONFIG_LOCALE: u32 = 0x0004;
    pub const CONFIG_TOUCHSCREEN: u32 = 0x0008;
    pub const CONFIG_KEYBOARD: u32 = 0x0010;
    pub const CONFIG_KEYBOARD_HIDDEN: u32 = 0x0020;
    pub const CONFIG_NAVIGATION: u32 = 0x0040;
    pub const CONFIG_ORIENTATION: u32 = 0x0080;
    pub const CONFIG_DENSITY: u32 = 0x0100;
    pub const CONFIG_SCREEN_SIZE: u32 = 0x0200;
    pub const CONFIG_VERSION: u32 = 0x0400;
    pub const CONFIG_SCREEN_LAYOUT: u32 = 0x0800;
    pub const CONFIG_UI_MODE: u32 = 0x1000;
    pub const CONFIG_SMALLEST_SCREEN_SIZE: u32 = 0x2000;
    pub const CONFIG_LAYOUTDIR: u32 = 0x4000;
    pub const CONFIG_SCREEN_ROUND: u32 = 0x8000;
    pub const CONFIG_COLOR_MODE: u32 = 0x10000;
    pub const CONFIG_GRAMMATICAL_GENDER: u32 = 0x20000;

    // -----------------------------------------------------------------------
    // Composite "union" views (little-endian, like aapt2 hosts)
    // -----------------------------------------------------------------------

    /// `imsi` union view: `mcc | mnc << 16`.
    pub fn imsi(&self) -> u32 {
        self.mcc as u32 | (self.mnc as u32) << 16
    }

    /// `locale` union view: the 4 language/country bytes as a LE u32.
    pub fn locale_u32(&self) -> u32 {
        u32::from_le_bytes([
            self.language[0],
            self.language[1],
            self.country[0],
            self.country[1],
        ])
    }

    /// `screenType` union view: `orientation | touchscreen << 8 | density << 16`.
    pub fn screen_type(&self) -> u32 {
        self.orientation as u32 | (self.touchscreen as u32) << 8 | (self.density as u32) << 16
    }

    /// `input` 24-bit union view: `keyboard | navigation << 8 | inputFlags << 16`
    /// (excludes `grammatical_inflection`, exactly like the C++ bitfield).
    pub fn input24(&self) -> u32 {
        self.keyboard as u32 | (self.navigation as u32) << 8 | (self.input_flags as u32) << 16
    }

    /// `screenSize` union view: `screenWidth | screenHeight << 16`.
    pub fn screen_size_u32(&self) -> u32 {
        self.screen_width as u32 | (self.screen_height as u32) << 16
    }

    /// `version` union view: `sdkVersion | minorVersion << 16`.
    pub fn version_u32(&self) -> u32 {
        self.sdk_version as u32 | (self.minor_version as u32) << 16
    }

    /// `screenConfig` union view: `screenLayout | uiMode << 8 | smallestScreenWidthDp << 16`.
    pub fn screen_config(&self) -> u32 {
        self.screen_layout as u32
            | (self.ui_mode as u32) << 8
            | (self.smallest_screen_width_dp as u32) << 16
    }

    /// `screenSizeDp` union view: `screenWidthDp | screenHeightDp << 16`.
    pub fn screen_size_dp_u32(&self) -> u32 {
        self.screen_width_dp as u32 | (self.screen_height_dp as u32) << 16
    }

    /// `screenConfig2` union view: `screenLayout2 | colorMode << 8 | screenConfigPad2 << 16`.
    pub fn screen_config2(&self) -> u32 {
        self.screen_layout2 as u32
            | (self.color_mode as u32) << 8
            | (self.screen_config_pad2 as u32) << 16
    }

    // -----------------------------------------------------------------------
    // Parsing (ConfigDescription::Parse)
    // -----------------------------------------------------------------------

    /// Parses a full resource qualifier string like `sw600dp-land-night-hdpi-v21`,
    /// `en-rUS`, `b+sr+Latn`, `mcc310-mnc004`, or `""` (the default config).
    ///
    /// The resulting configuration has the appropriate `sdk_version` applied
    /// for backwards compatibility (see [`Self::apply_version_for_compatibility`]).
    pub fn parse(s: &str) -> Option<ConfigDescription> {
        let mut config = ConfigDescription::default();
        if s.is_empty() {
            config.apply_version_for_compatibility();
            return Some(config);
        }

        // util::SplitAndLowercase(str, '-')
        let parts: Vec<String> = s.split('-').map(|p| p.to_ascii_lowercase()).collect();
        let n = parts.len();
        let mut i = 0usize;

        macro_rules! advance_or_succeed {
            () => {
                i += 1;
                if i == n {
                    config.apply_version_for_compatibility();
                    return Some(config);
                }
            };
        }

        if parse_mcc(&parts[i], &mut config) {
            advance_or_succeed!();
        }
        if parse_mnc(&parts[i], &mut config) {
            advance_or_succeed!();
        }

        // Locale spans a few '-' separators, so it controls the index.
        let (locale, consumed) = LocaleValue::init_from_parts(&parts[i..])?;
        if consumed > 0 {
            locale.write_to(&mut config);
            i += consumed;
            if i == n {
                config.apply_version_for_compatibility();
                return Some(config);
            }
        }

        // The exact parse* sequence from ConfigDescription::Parse.
        type Parser = fn(&str, &mut ConfigDescription) -> bool;
        const ORDERED: &[Parser] = &[
            parse_grammatical_inflection,
            parse_layout_direction,
            parse_smallest_screen_width_dp,
            parse_screen_width_dp,
            parse_screen_height_dp,
            parse_screen_layout_size,
            parse_screen_layout_long,
            parse_screen_round,
            parse_wide_color_gamut,
            parse_hdr,
            parse_orientation,
            parse_ui_mode_type,
            parse_ui_mode_night,
            parse_density,
            parse_touchscreen,
            parse_keys_hidden,
            parse_keyboard,
            parse_nav_hidden,
            parse_navigation,
            parse_screen_size,
            parse_version,
        ];
        for f in ORDERED {
            if f(&parts[i], &mut config) {
                advance_or_succeed!();
            }
        }

        // Unrecognized part remaining.
        None
    }

    /// If the configuration uses an axis that was added after the original
    /// Android release, makes sure the SDK version is set accordingly.
    /// Port of `ConfigDescription::ApplyVersionForCompatibility`.
    pub fn apply_version_for_compatibility(&mut self) {
        let min_sdk: u16 = if self.grammatical_inflection != 0 {
            SDK_U
        } else if (self.ui_mode & Self::MASK_UI_MODE_TYPE) == Self::UI_MODE_TYPE_VR_HEADSET
            || (self.color_mode & Self::MASK_WIDE_COLOR_GAMUT) != 0
            || (self.color_mode & Self::MASK_HDR) != 0
        {
            SDK_O
        } else if (self.screen_layout2 & Self::MASK_SCREENROUND) != 0 {
            SDK_MARSHMALLOW
        } else if self.density == Self::DENSITY_ANY {
            SDK_LOLLIPOP
        } else if self.smallest_screen_width_dp != Self::SCREENWIDTH_ANY
            || self.screen_width_dp != Self::SCREENWIDTH_ANY
            || self.screen_height_dp != Self::SCREENHEIGHT_ANY
        {
            SDK_HONEYCOMB_MR2
        } else if (self.ui_mode & Self::MASK_UI_MODE_TYPE) != Self::UI_MODE_TYPE_ANY
            || (self.ui_mode & Self::MASK_UI_MODE_NIGHT) != Self::UI_MODE_NIGHT_ANY
        {
            SDK_FROYO
        } else if (self.screen_layout & Self::MASK_SCREENSIZE) != Self::SCREENSIZE_ANY
            || (self.screen_layout & Self::MASK_SCREENLONG) != Self::SCREENLONG_ANY
            || self.density != Self::DENSITY_DEFAULT
        {
            SDK_DONUT
        } else {
            0
        };

        if min_sdk > self.sdk_version {
            self.sdk_version = min_sdk;
        }
    }

    /// Returns a copy of this config with `sdk_version` reset to 0.
    pub fn copy_without_sdk_version(&self) -> ConfigDescription {
        let mut copy = *self;
        copy.sdk_version = 0;
        copy
    }

    /// True if every field is zero (the default configuration).
    pub fn is_default(&self) -> bool {
        *self == ConfigDescription::default()
    }

    // -----------------------------------------------------------------------
    // Binary form
    // -----------------------------------------------------------------------

    /// Serializes to the 64-byte little-endian `ResTable_config` wire form,
    /// with the self-describing `size` field first. Equivalent to the struct
    /// layout after `swapHtoD` on a little-endian host.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; Self::SIZE];
        out[0..4].copy_from_slice(&(Self::SIZE as u32).to_le_bytes());
        out[4..6].copy_from_slice(&self.mcc.to_le_bytes());
        out[6..8].copy_from_slice(&self.mnc.to_le_bytes());
        out[8..10].copy_from_slice(&self.language);
        out[10..12].copy_from_slice(&self.country);
        out[12] = self.orientation;
        out[13] = self.touchscreen;
        out[14..16].copy_from_slice(&self.density.to_le_bytes());
        out[16] = self.keyboard;
        out[17] = self.navigation;
        out[18] = self.input_flags;
        out[19] = self.grammatical_inflection;
        out[20..22].copy_from_slice(&self.screen_width.to_le_bytes());
        out[22..24].copy_from_slice(&self.screen_height.to_le_bytes());
        out[24..26].copy_from_slice(&self.sdk_version.to_le_bytes());
        out[26..28].copy_from_slice(&self.minor_version.to_le_bytes());
        out[28] = self.screen_layout;
        out[29] = self.ui_mode;
        out[30..32].copy_from_slice(&self.smallest_screen_width_dp.to_le_bytes());
        out[32..34].copy_from_slice(&self.screen_width_dp.to_le_bytes());
        out[34..36].copy_from_slice(&self.screen_height_dp.to_le_bytes());
        out[36..40].copy_from_slice(&self.locale_script);
        out[40..48].copy_from_slice(&self.locale_variant);
        out[48] = self.screen_layout2;
        out[49] = self.color_mode;
        out[50..52].copy_from_slice(&self.screen_config_pad2.to_le_bytes());
        out[52] = self.locale_script_was_computed as u8;
        out[53..61].copy_from_slice(&self.locale_numbering_system);
        // out[61..64] is struct alignment padding, left zero.
        out
    }

    /// Deserializes a `ResTable_config` blob. Like `copyFromDtoH`, tolerates
    /// both smaller (older) and larger declared sizes: only
    /// `min(declared_size, data.len(), SIZE)` bytes are copied and the rest
    /// of the struct is zero-filled.
    pub fn from_bytes(data: &[u8]) -> Option<ConfigDescription> {
        if data.len() < 4 {
            return None;
        }
        let declared = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let n = declared.min(data.len()).min(Self::SIZE);
        let mut buf = [0u8; Self::SIZE];
        buf[..n].copy_from_slice(&data[..n]);

        let rd16 = |off: usize| u16::from_le_bytes([buf[off], buf[off + 1]]);
        let mut c = ConfigDescription {
            mcc: rd16(4),
            mnc: rd16(6),
            language: [buf[8], buf[9]],
            country: [buf[10], buf[11]],
            orientation: buf[12],
            touchscreen: buf[13],
            density: rd16(14),
            keyboard: buf[16],
            navigation: buf[17],
            input_flags: buf[18],
            grammatical_inflection: buf[19],
            screen_width: rd16(20),
            screen_height: rd16(22),
            sdk_version: rd16(24),
            minor_version: rd16(26),
            screen_layout: buf[28],
            ui_mode: buf[29],
            smallest_screen_width_dp: rd16(30),
            screen_width_dp: rd16(32),
            screen_height_dp: rd16(34),
            locale_script: [0; 4],
            locale_variant: [0; 8],
            screen_layout2: buf[48],
            color_mode: buf[49],
            screen_config_pad2: rd16(50),
            locale_script_was_computed: buf[52] != 0,
            locale_numbering_system: [0; 8],
        };
        c.locale_script.copy_from_slice(&buf[36..40]);
        c.locale_variant.copy_from_slice(&buf[40..48]);
        c.locale_numbering_system.copy_from_slice(&buf[53..61]);
        Some(c)
    }

    // -----------------------------------------------------------------------
    // Packed language/region codes
    // -----------------------------------------------------------------------

    /// Sets the language from the first up-to-3 bytes (2-letter codes must be
    /// followed by NUL or `-`).
    pub fn pack_language(&mut self, language: &[u8]) {
        self.language = pack_language_or_region(language, b'a');
    }

    /// Sets the region from the first up-to-3 bytes.
    pub fn pack_region(&mut self, region: &[u8]) {
        self.country = pack_language_or_region(region, b'0');
    }

    /// The 2- or 3-letter language code, or `""` when unset.
    pub fn unpack_language(&self) -> String {
        unpack_language_or_region(&self.language, b'a')
    }

    /// The 2-letter or 3-digit region code, or `""` when unset.
    pub fn unpack_region(&self) -> String {
        unpack_language_or_region(&self.country, b'0')
    }

    /// Clears every locale-related field.
    pub fn clear_locale(&mut self) {
        self.language = [0; 2];
        self.country = [0; 2];
        self.locale_script_was_computed = false;
        self.locale_script = [0; 4];
        self.locale_variant = [0; 8];
        self.locale_numbering_system = [0; 8];
    }

    /// Computes the likely script for the locale. Without the CLDR tables
    /// this always yields "unknown" (see module docs).
    pub fn compute_script(&mut self) {
        self.locale_script = locale_data_compute_script(&self.language, &self.country);
    }

    // -----------------------------------------------------------------------
    // Comparison
    // -----------------------------------------------------------------------

    /// Port of `ResTable_config::compare` (the resource-table sort order).
    pub fn compare(&self, o: &ConfigDescription) -> Ordering {
        let r = self.imsi().cmp(&o.imsi());
        if r != Ordering::Equal {
            return r;
        }
        let r = compare_locales(self, o);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.grammatical_inflection.cmp(&o.grammatical_inflection);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_type().cmp(&o.screen_type());
        if r != Ordering::Equal {
            return r;
        }
        let r = self.input24().cmp(&o.input24());
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_size_u32().cmp(&o.screen_size_u32());
        if r != Ordering::Equal {
            return r;
        }
        let r = self.version_u32().cmp(&o.version_u32());
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_layout.cmp(&o.screen_layout);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_layout2.cmp(&o.screen_layout2);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.color_mode.cmp(&o.color_mode);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.ui_mode.cmp(&o.ui_mode);
        if r != Ordering::Equal {
            return r;
        }
        let r = self
            .smallest_screen_width_dp
            .cmp(&o.smallest_screen_width_dp);
        if r != Ordering::Equal {
            return r;
        }
        self.screen_size_dp_u32().cmp(&o.screen_size_dp_u32())
    }

    /// Port of `ResTable_config::compareLogical` (human-meaningful order,
    /// used e.g. for sorted dump output).
    pub fn compare_logical(&self, o: &ConfigDescription) -> Ordering {
        let r = self.mcc.cmp(&o.mcc);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.mnc.cmp(&o.mnc);
        if r != Ordering::Equal {
            return r;
        }
        let r = compare_locales(self, o);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.grammatical_inflection.cmp(&o.grammatical_inflection);
        if r != Ordering::Equal {
            return r;
        }
        let r = (self.screen_layout & Self::MASK_LAYOUTDIR)
            .cmp(&(o.screen_layout & Self::MASK_LAYOUTDIR));
        if r != Ordering::Equal {
            return r;
        }
        let r = self
            .smallest_screen_width_dp
            .cmp(&o.smallest_screen_width_dp);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_width_dp.cmp(&o.screen_width_dp);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_height_dp.cmp(&o.screen_height_dp);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_width.cmp(&o.screen_width);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_height.cmp(&o.screen_height);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.density.cmp(&o.density);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.orientation.cmp(&o.orientation);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.touchscreen.cmp(&o.touchscreen);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.input24().cmp(&o.input24());
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_layout.cmp(&o.screen_layout);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.screen_layout2.cmp(&o.screen_layout2);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.color_mode.cmp(&o.color_mode);
        if r != Ordering::Equal {
            return r;
        }
        let r = self.ui_mode.cmp(&o.ui_mode);
        if r != Ordering::Equal {
            return r;
        }
        self.version_u32().cmp(&o.version_u32())
    }

    /// Port of `ResTable_config::diff`: returns the set of `CONFIG_*` bits
    /// for each value that differs.
    pub fn diff(&self, o: &ConfigDescription) -> u32 {
        let mut diffs = 0u32;
        if self.mcc != o.mcc {
            diffs |= Self::CONFIG_MCC;
        }
        if self.mnc != o.mnc {
            diffs |= Self::CONFIG_MNC;
        }
        if self.orientation != o.orientation {
            diffs |= Self::CONFIG_ORIENTATION;
        }
        if self.density != o.density {
            diffs |= Self::CONFIG_DENSITY;
        }
        if self.touchscreen != o.touchscreen {
            diffs |= Self::CONFIG_TOUCHSCREEN;
        }
        if ((self.input_flags ^ o.input_flags) & (Self::MASK_KEYSHIDDEN | Self::MASK_NAVHIDDEN))
            != 0
        {
            diffs |= Self::CONFIG_KEYBOARD_HIDDEN;
        }
        if self.keyboard != o.keyboard {
            diffs |= Self::CONFIG_KEYBOARD;
        }
        if self.navigation != o.navigation {
            diffs |= Self::CONFIG_NAVIGATION;
        }
        if self.screen_size_u32() != o.screen_size_u32() {
            diffs |= Self::CONFIG_SCREEN_SIZE;
        }
        if self.version_u32() != o.version_u32() {
            diffs |= Self::CONFIG_VERSION;
        }
        if (self.screen_layout & Self::MASK_LAYOUTDIR) != (o.screen_layout & Self::MASK_LAYOUTDIR) {
            diffs |= Self::CONFIG_LAYOUTDIR;
        }
        if (self.screen_layout & !Self::MASK_LAYOUTDIR) != (o.screen_layout & !Self::MASK_LAYOUTDIR)
        {
            diffs |= Self::CONFIG_SCREEN_LAYOUT;
        }
        if (self.screen_layout2 & Self::MASK_SCREENROUND)
            != (o.screen_layout2 & Self::MASK_SCREENROUND)
        {
            diffs |= Self::CONFIG_SCREEN_ROUND;
        }
        if (self.color_mode & Self::MASK_WIDE_COLOR_GAMUT)
            != (o.color_mode & Self::MASK_WIDE_COLOR_GAMUT)
        {
            diffs |= Self::CONFIG_COLOR_MODE;
        }
        if (self.color_mode & Self::MASK_HDR) != (o.color_mode & Self::MASK_HDR) {
            diffs |= Self::CONFIG_COLOR_MODE;
        }
        if self.ui_mode != o.ui_mode {
            diffs |= Self::CONFIG_UI_MODE;
        }
        if self.smallest_screen_width_dp != o.smallest_screen_width_dp {
            diffs |= Self::CONFIG_SMALLEST_SCREEN_SIZE;
        }
        if self.screen_size_dp_u32() != o.screen_size_dp_u32() {
            diffs |= Self::CONFIG_SCREEN_SIZE;
        }
        if self.grammatical_inflection != o.grammatical_inflection {
            diffs |= Self::CONFIG_GRAMMATICAL_GENDER;
        }
        if compare_locales(self, o) != Ordering::Equal {
            diffs |= Self::CONFIG_LOCALE;
        }
        diffs
    }

    /// Importance score of the locale (variants > explicit scripts >
    /// numbering systems).
    pub fn get_importance_score_of_locale(&self) -> i32 {
        (if self.locale_variant[0] != 0 { 4 } else { 0 })
            + (if self.locale_script[0] != 0 && !self.locale_script_was_computed {
                2
            } else {
                0
            })
            + (if self.locale_numbering_system[0] != 0 {
                1
            } else {
                0
            })
    }

    /// Positive if `self` is more locale-specific than `o`, negative if `o`
    /// is, 0 if equally specific.
    pub fn is_locale_more_specific_than(&self, o: &ConfigDescription) -> i32 {
        if self.locale_u32() != 0 || o.locale_u32() != 0 {
            if self.language[0] != o.language[0] {
                if self.language[0] == 0 {
                    return -1;
                }
                if o.language[0] == 0 {
                    return 1;
                }
            }
            if self.country[0] != o.country[0] {
                if self.country[0] == 0 {
                    return -1;
                }
                if o.country[0] == 0 {
                    return 1;
                }
            }
        }
        self.get_importance_score_of_locale() - o.get_importance_score_of_locale()
    }

    /// Port of `ResTable_config::isMoreSpecificThan`.
    pub fn is_more_specific_than(&self, o: &ConfigDescription) -> bool {
        // The order of the following tests defines the importance of one
        // configuration parameter over another.
        if self.imsi() != 0 || o.imsi() != 0 {
            if self.mcc != o.mcc {
                if self.mcc == 0 {
                    return false;
                }
                if o.mcc == 0 {
                    return true;
                }
            }
            if self.mnc != o.mnc {
                if self.mnc == 0 {
                    return false;
                }
                if o.mnc == 0 {
                    return true;
                }
            }
        }

        if self.locale_u32() != 0 || o.locale_u32() != 0 {
            let diff = self.is_locale_more_specific_than(o);
            if diff < 0 {
                return false;
            }
            if diff > 0 {
                return true;
            }
        }

        if (self.grammatical_inflection != 0 || o.grammatical_inflection != 0)
            && self.grammatical_inflection != o.grammatical_inflection
        {
            if self.grammatical_inflection == 0 {
                return false;
            }
            if o.grammatical_inflection == 0 {
                return true;
            }
        }

        if (self.screen_layout != 0 || o.screen_layout != 0)
            && ((self.screen_layout ^ o.screen_layout) & Self::MASK_LAYOUTDIR) != 0
        {
            if (self.screen_layout & Self::MASK_LAYOUTDIR) == 0 {
                return false;
            }
            if (o.screen_layout & Self::MASK_LAYOUTDIR) == 0 {
                return true;
            }
        }

        if (self.smallest_screen_width_dp != 0 || o.smallest_screen_width_dp != 0)
            && self.smallest_screen_width_dp != o.smallest_screen_width_dp
        {
            if self.smallest_screen_width_dp == 0 {
                return false;
            }
            if o.smallest_screen_width_dp == 0 {
                return true;
            }
        }

        if self.screen_size_dp_u32() != 0 || o.screen_size_dp_u32() != 0 {
            if self.screen_width_dp != o.screen_width_dp {
                if self.screen_width_dp == 0 {
                    return false;
                }
                if o.screen_width_dp == 0 {
                    return true;
                }
            }
            if self.screen_height_dp != o.screen_height_dp {
                if self.screen_height_dp == 0 {
                    return false;
                }
                if o.screen_height_dp == 0 {
                    return true;
                }
            }
        }

        if self.screen_layout != 0 || o.screen_layout != 0 {
            if ((self.screen_layout ^ o.screen_layout) & Self::MASK_SCREENSIZE) != 0 {
                if (self.screen_layout & Self::MASK_SCREENSIZE) == 0 {
                    return false;
                }
                if (o.screen_layout & Self::MASK_SCREENSIZE) == 0 {
                    return true;
                }
            }
            if ((self.screen_layout ^ o.screen_layout) & Self::MASK_SCREENLONG) != 0 {
                if (self.screen_layout & Self::MASK_SCREENLONG) == 0 {
                    return false;
                }
                if (o.screen_layout & Self::MASK_SCREENLONG) == 0 {
                    return true;
                }
            }
        }

        if (self.screen_layout2 != 0 || o.screen_layout2 != 0)
            && ((self.screen_layout2 ^ o.screen_layout2) & Self::MASK_SCREENROUND) != 0
        {
            if (self.screen_layout2 & Self::MASK_SCREENROUND) == 0 {
                return false;
            }
            if (o.screen_layout2 & Self::MASK_SCREENROUND) == 0 {
                return true;
            }
        }

        if self.color_mode != 0 || o.color_mode != 0 {
            if ((self.color_mode ^ o.color_mode) & Self::MASK_HDR) != 0 {
                if (self.color_mode & Self::MASK_HDR) == 0 {
                    return false;
                }
                if (o.color_mode & Self::MASK_HDR) == 0 {
                    return true;
                }
            }
            if ((self.color_mode ^ o.color_mode) & Self::MASK_WIDE_COLOR_GAMUT) != 0 {
                if (self.color_mode & Self::MASK_WIDE_COLOR_GAMUT) == 0 {
                    return false;
                }
                if (o.color_mode & Self::MASK_WIDE_COLOR_GAMUT) == 0 {
                    return true;
                }
            }
        }

        if self.orientation != o.orientation {
            if self.orientation == 0 {
                return false;
            }
            if o.orientation == 0 {
                return true;
            }
        }

        if self.ui_mode != 0 || o.ui_mode != 0 {
            if ((self.ui_mode ^ o.ui_mode) & Self::MASK_UI_MODE_TYPE) != 0 {
                if (self.ui_mode & Self::MASK_UI_MODE_TYPE) == 0 {
                    return false;
                }
                if (o.ui_mode & Self::MASK_UI_MODE_TYPE) == 0 {
                    return true;
                }
            }
            if ((self.ui_mode ^ o.ui_mode) & Self::MASK_UI_MODE_NIGHT) != 0 {
                if (self.ui_mode & Self::MASK_UI_MODE_NIGHT) == 0 {
                    return false;
                }
                if (o.ui_mode & Self::MASK_UI_MODE_NIGHT) == 0 {
                    return true;
                }
            }
        }

        // density is never 'more specific' as the default just equals 160.

        if self.touchscreen != o.touchscreen {
            if self.touchscreen == 0 {
                return false;
            }
            if o.touchscreen == 0 {
                return true;
            }
        }

        if self.input24() != 0 || o.input24() != 0 {
            if ((self.input_flags ^ o.input_flags) & Self::MASK_KEYSHIDDEN) != 0 {
                if (self.input_flags & Self::MASK_KEYSHIDDEN) == 0 {
                    return false;
                }
                if (o.input_flags & Self::MASK_KEYSHIDDEN) == 0 {
                    return true;
                }
            }
            if ((self.input_flags ^ o.input_flags) & Self::MASK_NAVHIDDEN) != 0 {
                if (self.input_flags & Self::MASK_NAVHIDDEN) == 0 {
                    return false;
                }
                if (o.input_flags & Self::MASK_NAVHIDDEN) == 0 {
                    return true;
                }
            }
            if self.keyboard != o.keyboard {
                if self.keyboard == 0 {
                    return false;
                }
                if o.keyboard == 0 {
                    return true;
                }
            }
            if self.navigation != o.navigation {
                if self.navigation == 0 {
                    return false;
                }
                if o.navigation == 0 {
                    return true;
                }
            }
        }

        if self.screen_size_u32() != 0 || o.screen_size_u32() != 0 {
            if self.screen_width != o.screen_width {
                if self.screen_width == 0 {
                    return false;
                }
                if o.screen_width == 0 {
                    return true;
                }
            }
            if self.screen_height != o.screen_height {
                if self.screen_height == 0 {
                    return false;
                }
                if o.screen_height == 0 {
                    return true;
                }
            }
        }

        if self.version_u32() != 0 || o.version_u32() != 0 {
            if self.sdk_version != o.sdk_version {
                if self.sdk_version == 0 {
                    return false;
                }
                if o.sdk_version == 0 {
                    return true;
                }
            }
            if self.minor_version != o.minor_version {
                if self.minor_version == 0 {
                    return false;
                }
                if o.minor_version == 0 {
                    return true;
                }
            }
        }
        false
    }

    /// Port of `ResTable_config::isLocaleBetterThan`: true if `self` is a
    /// better locale match than `o` for `requested`. Assumes non-matching
    /// locales were filtered out by [`Self::matches`] beforehand.
    pub fn is_locale_better_than(
        &self,
        o: &ConfigDescription,
        requested: &ConfigDescription,
    ) -> bool {
        if requested.locale_u32() == 0 {
            // The request doesn't have a locale, so no resource is better
            // than the other.
            return false;
        }

        if self.locale_u32() == 0 && o.locale_u32() == 0 {
            // The locale part of both resources is empty, so none is better
            // than the other.
            return false;
        }

        if !langs_are_equivalent(&self.language, &o.language) {
            // We consider the one that has the language specified a better
            // match, except that no-language resources win for US English
            // and locales close to it.
            if are_identical(&requested.language, &K_ENGLISH) {
                if are_identical(&requested.country, &K_UNITED_STATES) {
                    // For US English itself, a no-locale resource is better
                    // if the other resource has a non-US country.
                    if self.language[0] != 0 {
                        return self.country[0] == 0
                            || are_identical(&self.country, &K_UNITED_STATES);
                    } else {
                        return !(o.country[0] == 0 || are_identical(&o.country, &K_UNITED_STATES));
                    }
                } else if locale_data_is_close_to_us_english(&requested.country) {
                    if self.language[0] != 0 {
                        return locale_data_is_close_to_us_english(&self.country);
                    } else {
                        return !locale_data_is_close_to_us_english(&o.country);
                    }
                }
            }
            return self.language[0] != 0;
        }

        // Both resources have an equivalent non-empty language to the
        // request; check regions.
        let region_comparison = locale_data_compare_regions(
            &self.country,
            &o.country,
            &requested.language,
            &requested.locale_script,
            &requested.country,
        );
        if region_comparison != 0 {
            return region_comparison > 0;
        }

        // The regions are the same. Try the variant.
        let locale_matches = self.locale_variant == requested.locale_variant;
        let other_matches = o.locale_variant == requested.locale_variant;
        if locale_matches != other_matches {
            return locale_matches;
        }

        // The variants are the same, try numbering system.
        let locale_numsys_matches =
            self.locale_numbering_system == requested.locale_numbering_system;
        let other_numsys_matches = o.locale_numbering_system == requested.locale_numbering_system;
        if locale_numsys_matches != other_numsys_matches {
            return locale_numsys_matches;
        }

        // Finally, the languages, although equivalent, may still be
        // different (like Tagalog vs Filipino). Identical beats equivalent.
        if are_identical(&self.language, &requested.language)
            && !are_identical(&o.language, &requested.language)
        {
            return true;
        }

        false
    }

    /// Port of `ResTable_config::isBetterThanBeforeLocale`.
    pub fn is_better_than_before_locale(
        &self,
        o: &ConfigDescription,
        requested: Option<&ConfigDescription>,
    ) -> bool {
        if let Some(req) = requested {
            if self.imsi() != 0 || o.imsi() != 0 {
                if self.mcc != o.mcc && req.mcc != 0 {
                    return self.mcc != 0;
                }
                if self.mnc != o.mnc && req.mnc != 0 {
                    return self.mnc != 0;
                }
            }
        }
        false
    }

    /// Port of `ResTable_config::isBetterThan`: true if `self` is a better
    /// match than `o` for `requested`. With `requested == None` this is
    /// `is_more_specific_than`.
    pub fn is_better_than(
        &self,
        o: &ConfigDescription,
        requested: Option<&ConfigDescription>,
    ) -> bool {
        let req = match requested {
            Some(r) => r,
            None => return self.is_more_specific_than(o),
        };

        if self.imsi() != 0 || o.imsi() != 0 {
            if self.mcc != o.mcc && req.mcc != 0 {
                return self.mcc != 0;
            }
            if self.mnc != o.mnc && req.mnc != 0 {
                return self.mnc != 0;
            }
        }

        if req.locale_u32() != 0
            && (self.locale_u32() != 0 || o.locale_u32() != 0)
            && self.is_locale_better_than(o, req)
        {
            return true;
        }

        if (self.grammatical_inflection != 0 || o.grammatical_inflection != 0)
            && self.grammatical_inflection != o.grammatical_inflection
            && req.grammatical_inflection != 0
        {
            return self.grammatical_inflection != 0;
        }

        if (self.screen_layout != 0 || o.screen_layout != 0)
            && ((self.screen_layout ^ o.screen_layout) & Self::MASK_LAYOUTDIR) != 0
            && (req.screen_layout & Self::MASK_LAYOUTDIR) != 0
        {
            let my_layout_dir = self.screen_layout & Self::MASK_LAYOUTDIR;
            let o_layout_dir = o.screen_layout & Self::MASK_LAYOUTDIR;
            return my_layout_dir > o_layout_dir;
        }

        if self.smallest_screen_width_dp != 0 || o.smallest_screen_width_dp != 0 {
            // The configuration closest to the actual size is best. We
            // assume larger configs have already been filtered out, so we
            // just want the largest one here.
            if self.smallest_screen_width_dp != o.smallest_screen_width_dp {
                return self.smallest_screen_width_dp > o.smallest_screen_width_dp;
            }
        }

        if self.screen_size_dp_u32() != 0 || o.screen_size_dp_u32() != 0 {
            // "Better" is based on the sum of the difference between both
            // width and height from the requested dimensions.
            let mut my_delta = 0i32;
            let mut other_delta = 0i32;
            if req.screen_width_dp != 0 {
                my_delta += req.screen_width_dp as i32 - self.screen_width_dp as i32;
                other_delta += req.screen_width_dp as i32 - o.screen_width_dp as i32;
            }
            if req.screen_height_dp != 0 {
                my_delta += req.screen_height_dp as i32 - self.screen_height_dp as i32;
                other_delta += req.screen_height_dp as i32 - o.screen_height_dp as i32;
            }
            if my_delta != other_delta {
                return my_delta < other_delta;
            }
        }

        if self.screen_layout != 0 || o.screen_layout != 0 {
            if ((self.screen_layout ^ o.screen_layout) & Self::MASK_SCREENSIZE) != 0
                && (req.screen_layout & Self::MASK_SCREENSIZE) != 0
            {
                // A little backwards compatibility here: undefined is
                // considered equivalent to normal. But only if the
                // requested size is at least normal; otherwise, small is
                // better than the default.
                let my_sl = (self.screen_layout & Self::MASK_SCREENSIZE) as i32;
                let o_sl = (o.screen_layout & Self::MASK_SCREENSIZE) as i32;
                let mut fixed_my_sl = my_sl;
                let mut fixed_o_sl = o_sl;
                if (req.screen_layout & Self::MASK_SCREENSIZE) >= Self::SCREENSIZE_NORMAL {
                    if fixed_my_sl == 0 {
                        fixed_my_sl = Self::SCREENSIZE_NORMAL as i32;
                    }
                    if fixed_o_sl == 0 {
                        fixed_o_sl = Self::SCREENSIZE_NORMAL as i32;
                    }
                }
                // The best match is the one closest to the requested screen
                // size, but not over (the not-over part is dealt with in
                // match() below).
                if fixed_my_sl == fixed_o_sl {
                    // If the two are the same, but 'this' is actually
                    // undefined, then the other is really a better match.
                    return my_sl != 0;
                }
                return fixed_my_sl > fixed_o_sl;
            }
            if ((self.screen_layout ^ o.screen_layout) & Self::MASK_SCREENLONG) != 0
                && (req.screen_layout & Self::MASK_SCREENLONG) != 0
            {
                return (self.screen_layout & Self::MASK_SCREENLONG) != 0;
            }
        }

        if (self.screen_layout2 != 0 || o.screen_layout2 != 0)
            && ((self.screen_layout2 ^ o.screen_layout2) & Self::MASK_SCREENROUND) != 0
            && (req.screen_layout2 & Self::MASK_SCREENROUND) != 0
        {
            return (self.screen_layout2 & Self::MASK_SCREENROUND) != 0;
        }

        if self.color_mode != 0 || o.color_mode != 0 {
            if ((self.color_mode ^ o.color_mode) & Self::MASK_WIDE_COLOR_GAMUT) != 0
                && (req.color_mode & Self::MASK_WIDE_COLOR_GAMUT) != 0
            {
                return (self.color_mode & Self::MASK_WIDE_COLOR_GAMUT) != 0;
            }
            if ((self.color_mode ^ o.color_mode) & Self::MASK_HDR) != 0
                && (req.color_mode & Self::MASK_HDR) != 0
            {
                return (self.color_mode & Self::MASK_HDR) != 0;
            }
        }

        if self.orientation != o.orientation && req.orientation != 0 {
            return self.orientation != 0;
        }

        if self.ui_mode != 0 || o.ui_mode != 0 {
            if ((self.ui_mode ^ o.ui_mode) & Self::MASK_UI_MODE_TYPE) != 0
                && (req.ui_mode & Self::MASK_UI_MODE_TYPE) != 0
            {
                return (self.ui_mode & Self::MASK_UI_MODE_TYPE) != 0;
            }
            if ((self.ui_mode ^ o.ui_mode) & Self::MASK_UI_MODE_NIGHT) != 0
                && (req.ui_mode & Self::MASK_UI_MODE_NIGHT) != 0
            {
                return (self.ui_mode & Self::MASK_UI_MODE_NIGHT) != 0;
            }
        }

        if self.screen_type() != 0 || o.screen_type() != 0 {
            if self.density != o.density {
                // Use the system default density (DENSITY_MEDIUM, 160dpi)
                // if none specified.
                let this_density = if self.density != 0 {
                    self.density as i32
                } else {
                    Self::DENSITY_MEDIUM as i32
                };
                let other_density = if o.density != 0 {
                    o.density as i32
                } else {
                    Self::DENSITY_MEDIUM as i32
                };

                // We always prefer DENSITY_ANY over scaling a density bucket.
                if this_density == Self::DENSITY_ANY as i32 {
                    return true;
                } else if other_density == Self::DENSITY_ANY as i32 {
                    return false;
                }

                let mut requested_density = req.density as i32;
                if req.density == 0 || req.density == Self::DENSITY_ANY {
                    requested_density = Self::DENSITY_MEDIUM as i32;
                }

                // DENSITY_ANY is now dealt with. Pick a density bucket and
                // potentially scale it. Always prefer scaling down.
                let mut h = this_density;
                let mut l = other_density;
                let mut im_bigger = true;
                if l > h {
                    std::mem::swap(&mut l, &mut h);
                    im_bigger = false;
                }

                if h == requested_density {
                    // This also handles l == h == requestedDensity.
                    return im_bigger;
                } else if l >= requested_density {
                    // Requested value lower than both l and h: give l.
                    return !im_bigger;
                } else {
                    // Otherwise give h.
                    return im_bigger;
                }
            }

            if self.touchscreen != o.touchscreen && req.touchscreen != 0 {
                return self.touchscreen != 0;
            }
        }

        if self.input24() != 0 || o.input24() != 0 {
            let keys_hidden = self.input_flags & Self::MASK_KEYSHIDDEN;
            let o_keys_hidden = o.input_flags & Self::MASK_KEYSHIDDEN;
            if keys_hidden != o_keys_hidden {
                let req_keys_hidden = req.input_flags & Self::MASK_KEYSHIDDEN;
                if req_keys_hidden != 0 {
                    if keys_hidden == 0 {
                        return false;
                    }
                    if o_keys_hidden == 0 {
                        return true;
                    }
                    // For compatibility, we count KEYSHIDDEN_NO as being the
                    // same as KEYSHIDDEN_SOFT. Disambiguate by making an
                    // exact match more specific.
                    if req_keys_hidden == keys_hidden {
                        return true;
                    }
                    if req_keys_hidden == o_keys_hidden {
                        return false;
                    }
                }
            }

            let nav_hidden = self.input_flags & Self::MASK_NAVHIDDEN;
            let o_nav_hidden = o.input_flags & Self::MASK_NAVHIDDEN;
            if nav_hidden != o_nav_hidden {
                let req_nav_hidden = req.input_flags & Self::MASK_NAVHIDDEN;
                if req_nav_hidden != 0 {
                    if nav_hidden == 0 {
                        return false;
                    }
                    if o_nav_hidden == 0 {
                        return true;
                    }
                }
            }

            if self.keyboard != o.keyboard && req.keyboard != 0 {
                return self.keyboard != 0;
            }

            if self.navigation != o.navigation && req.navigation != 0 {
                return self.navigation != 0;
            }
        }

        if self.screen_size_u32() != 0 || o.screen_size_u32() != 0 {
            // "Better" is based on the sum of the difference between both
            // width and height from the requested dimensions.
            let mut my_delta = 0i32;
            let mut other_delta = 0i32;
            if req.screen_width != 0 {
                my_delta += req.screen_width as i32 - self.screen_width as i32;
                other_delta += req.screen_width as i32 - o.screen_width as i32;
            }
            if req.screen_height != 0 {
                my_delta += req.screen_height as i32 - self.screen_height as i32;
                other_delta += req.screen_height as i32 - o.screen_height as i32;
            }
            if my_delta != other_delta {
                return my_delta < other_delta;
            }
        }

        if self.version_u32() != 0 || o.version_u32() != 0 {
            if self.sdk_version != o.sdk_version && req.sdk_version != 0 {
                return self.sdk_version > o.sdk_version;
            }
            if self.minor_version != o.minor_version && req.minor_version != 0 {
                return self.minor_version != 0;
            }
        }

        false
    }

    /// Port of `ResTable_config::match`: true if `self` can be considered a
    /// match for the parameters in `settings`. Note this is asymmetric: a
    /// default piece of data matches every request, but a request for the
    /// default should not match odd specifics.
    pub fn matches(&self, settings: &ConfigDescription) -> bool {
        if self.imsi() != 0 {
            if self.mcc != 0 && self.mcc != settings.mcc {
                return false;
            }
            if self.mnc != 0 && self.mnc != settings.mnc {
                return false;
            }
        }
        if self.locale_u32() != 0 {
            // Don't consider country and variants when deciding matches;
            // those are weeded out in the is_more_specific_than test.
            if !langs_are_equivalent(&self.language, &settings.language) {
                return false;
            }

            // For backward compatibility and supporting private-use locales,
            // fall back to old behavior if we couldn't determine the script
            // for either the desired or the provided locale. If we could
            // determine the scripts, they must match.
            let mut countries_must_match = false;
            let mut script = [0u8; 4];
            if settings.locale_script[0] == 0 {
                // Could not determine the request's script.
                countries_must_match = true;
            } else if self.locale_script[0] == 0 && !self.locale_script_was_computed {
                // Script was not provided or computed, so we try to compute
                // it (always unknown without the CLDR tables — see module
                // docs).
                let computed = locale_data_compute_script(&self.language, &self.country);
                if computed[0] == 0 {
                    countries_must_match = true;
                } else {
                    script = computed;
                }
            } else {
                // Script was provided, so just use it.
                script = self.locale_script;
            }

            if countries_must_match {
                if self.country[0] != 0 && !are_identical(&self.country, &settings.country) {
                    return false;
                }
            } else if script != settings.locale_script {
                return false;
            }
        }

        if self.grammatical_inflection != 0
            && self.grammatical_inflection != settings.grammatical_inflection
        {
            return false;
        }

        if self.screen_config() != 0 {
            let layout_dir = self.screen_layout & Self::MASK_LAYOUTDIR;
            let set_layout_dir = settings.screen_layout & Self::MASK_LAYOUTDIR;
            if layout_dir != 0 && layout_dir != set_layout_dir {
                return false;
            }

            let screen_size = self.screen_layout & Self::MASK_SCREENSIZE;
            let set_screen_size = settings.screen_layout & Self::MASK_SCREENSIZE;
            // Any screen sizes for larger screens than the setting do not
            // match.
            if screen_size != 0 && screen_size > set_screen_size {
                return false;
            }

            let screen_long = self.screen_layout & Self::MASK_SCREENLONG;
            let set_screen_long = settings.screen_layout & Self::MASK_SCREENLONG;
            if screen_long != 0 && screen_long != set_screen_long {
                return false;
            }

            let ui_mode_type = self.ui_mode & Self::MASK_UI_MODE_TYPE;
            let set_ui_mode_type = settings.ui_mode & Self::MASK_UI_MODE_TYPE;
            if ui_mode_type != 0 && ui_mode_type != set_ui_mode_type {
                return false;
            }

            let ui_mode_night = self.ui_mode & Self::MASK_UI_MODE_NIGHT;
            let set_ui_mode_night = settings.ui_mode & Self::MASK_UI_MODE_NIGHT;
            if ui_mode_night != 0 && ui_mode_night != set_ui_mode_night {
                return false;
            }

            if self.smallest_screen_width_dp != 0
                && self.smallest_screen_width_dp > settings.smallest_screen_width_dp
            {
                return false;
            }
        }

        if self.screen_config2() != 0 {
            let screen_round = self.screen_layout2 & Self::MASK_SCREENROUND;
            let set_screen_round = settings.screen_layout2 & Self::MASK_SCREENROUND;
            if screen_round != 0 && screen_round != set_screen_round {
                return false;
            }

            let hdr = self.color_mode & Self::MASK_HDR;
            let set_hdr = settings.color_mode & Self::MASK_HDR;
            if hdr != 0 && hdr != set_hdr {
                return false;
            }

            let wide_color_gamut = self.color_mode & Self::MASK_WIDE_COLOR_GAMUT;
            let set_wide_color_gamut = settings.color_mode & Self::MASK_WIDE_COLOR_GAMUT;
            if wide_color_gamut != 0 && wide_color_gamut != set_wide_color_gamut {
                return false;
            }
        }

        if self.screen_size_dp_u32() != 0 {
            if self.screen_width_dp != 0 && self.screen_width_dp > settings.screen_width_dp {
                return false;
            }
            if self.screen_height_dp != 0 && self.screen_height_dp > settings.screen_height_dp {
                return false;
            }
        }

        if self.screen_type() != 0 {
            if self.orientation != 0 && self.orientation != settings.orientation {
                return false;
            }
            // density always matches - we can scale it. See is_better_than.
            if self.touchscreen != 0 && self.touchscreen != settings.touchscreen {
                return false;
            }
        }

        if self.input24() != 0 {
            let keys_hidden = self.input_flags & Self::MASK_KEYSHIDDEN;
            let set_keys_hidden = settings.input_flags & Self::MASK_KEYSHIDDEN;
            if keys_hidden != 0 && keys_hidden != set_keys_hidden {
                // For compatibility, we count a request for KEYSHIDDEN_NO as
                // also matching the more recent KEYSHIDDEN_SOFT. Basically
                // KEYSHIDDEN_NO means there is some kind of keyboard
                // available.
                if keys_hidden != Self::KEYSHIDDEN_NO || set_keys_hidden != Self::KEYSHIDDEN_SOFT {
                    return false;
                }
            }
            let nav_hidden = self.input_flags & Self::MASK_NAVHIDDEN;
            let set_nav_hidden = settings.input_flags & Self::MASK_NAVHIDDEN;
            if nav_hidden != 0 && nav_hidden != set_nav_hidden {
                return false;
            }
            if self.keyboard != 0 && self.keyboard != settings.keyboard {
                return false;
            }
            if self.navigation != 0 && self.navigation != settings.navigation {
                return false;
            }
        }

        if self.screen_size_u32() != 0 {
            if self.screen_width != 0 && self.screen_width > settings.screen_width {
                return false;
            }
            if self.screen_height != 0 && self.screen_height > settings.screen_height {
                return false;
            }
        }

        if self.version_u32() != 0 {
            if self.sdk_version != 0 && self.sdk_version > settings.sdk_version {
                return false;
            }
            if self.minor_version != 0 && self.minor_version != settings.minor_version {
                return false;
            }
        }

        true
    }

    /// `MatchWithDensity`: like [`Self::matches`], but a density of 0 in
    /// `self` only matches an `o` that also specifies a density of nonzero.
    pub fn match_with_density(&self, o: &ConfigDescription) -> bool {
        self.matches(o) && (self.density == 0 || o.density != 0)
    }

    // -----------------------------------------------------------------------
    // aapt2-level helpers (ConfigDescription.cpp)
    // -----------------------------------------------------------------------

    /// A configuration X dominates Y if X has at least the precedence of Y
    /// and X is strictly more general than Y. Port of
    /// `ConfigDescription::Dominates`.
    pub fn dominates(&self, o: &ConfigDescription) -> bool {
        if self.compare(o) == Ordering::Equal {
            return true;
        }

        // Locale de-duping is non-trivial, disabled (b/62409213). De-duping
        // is also disabled for all configuration qualifiers with precedence
        // higher than locale (b/171892595).
        if self.diff(o) & (Self::CONFIG_LOCALE | Self::CONFIG_MCC | Self::CONFIG_MNC) != 0 {
            return false;
        }

        if self.compare(&ConfigDescription::default()) == Ordering::Equal {
            return true;
        }

        self.match_with_density(o)
            && !o.match_with_density(self)
            && !self.is_more_specific_than(o)
            && !o.has_higher_precedence_than(self)
    }

    /// True if this configuration defines a more important configuration
    /// parameter than `o`. Port of
    /// `ConfigDescription::HasHigherPrecedenceThan` — the ordering matches
    /// `ResTable_config::isBetterThan`.
    pub fn has_higher_precedence_than(&self, o: &ConfigDescription) -> bool {
        if self.mcc != 0 || o.mcc != 0 {
            return o.mcc == 0;
        }
        if self.mnc != 0 || o.mnc != 0 {
            return o.mnc == 0;
        }
        if self.language[0] != 0 || o.language[0] != 0 {
            return o.language[0] == 0;
        }
        if self.country[0] != 0 || o.country[0] != 0 {
            return o.country[0] == 0;
        }
        // Script and variant require either a language or country, both of
        // which have higher precedence.
        if self.grammatical_inflection != 0 || o.grammatical_inflection != 0 {
            return o.grammatical_inflection == 0;
        }
        if ((self.screen_layout | o.screen_layout) & Self::MASK_LAYOUTDIR) != 0 {
            return (o.screen_layout & Self::MASK_LAYOUTDIR) == 0;
        }
        if self.smallest_screen_width_dp != 0 || o.smallest_screen_width_dp != 0 {
            return o.smallest_screen_width_dp == 0;
        }
        if self.screen_width_dp != 0 || o.screen_width_dp != 0 {
            return o.screen_width_dp == 0;
        }
        if self.screen_height_dp != 0 || o.screen_height_dp != 0 {
            return o.screen_height_dp == 0;
        }
        if ((self.screen_layout | o.screen_layout) & Self::MASK_SCREENSIZE) != 0 {
            return (o.screen_layout & Self::MASK_SCREENSIZE) == 0;
        }
        if ((self.screen_layout | o.screen_layout) & Self::MASK_SCREENLONG) != 0 {
            return (o.screen_layout & Self::MASK_SCREENLONG) == 0;
        }
        if ((self.screen_layout2 | o.screen_layout2) & Self::MASK_SCREENROUND) != 0 {
            return (o.screen_layout2 & Self::MASK_SCREENROUND) == 0;
        }
        if ((self.color_mode | o.color_mode) & Self::MASK_HDR) != 0 {
            return (o.color_mode & Self::MASK_HDR) == 0;
        }
        if ((self.color_mode | o.color_mode) & Self::MASK_WIDE_COLOR_GAMUT) != 0 {
            return (o.color_mode & Self::MASK_WIDE_COLOR_GAMUT) == 0;
        }
        if self.orientation != 0 || o.orientation != 0 {
            return o.orientation == 0;
        }
        if ((self.ui_mode | o.ui_mode) & Self::MASK_UI_MODE_TYPE) != 0 {
            return (o.ui_mode & Self::MASK_UI_MODE_TYPE) == 0;
        }
        if ((self.ui_mode | o.ui_mode) & Self::MASK_UI_MODE_NIGHT) != 0 {
            return (o.ui_mode & Self::MASK_UI_MODE_NIGHT) == 0;
        }
        if self.density != 0 || o.density != 0 {
            return o.density == 0;
        }
        if self.touchscreen != 0 || o.touchscreen != 0 {
            return o.touchscreen == 0;
        }
        if ((self.input_flags | o.input_flags) & Self::MASK_KEYSHIDDEN) != 0 {
            return (o.input_flags & Self::MASK_KEYSHIDDEN) == 0;
        }
        if ((self.input_flags | o.input_flags) & Self::MASK_NAVHIDDEN) != 0 {
            return (o.input_flags & Self::MASK_NAVHIDDEN) == 0;
        }
        if self.keyboard != 0 || o.keyboard != 0 {
            return o.keyboard == 0;
        }
        if self.navigation != 0 || o.navigation != 0 {
            return o.navigation == 0;
        }
        if self.screen_width != 0 || o.screen_width != 0 {
            return o.screen_width == 0;
        }
        if self.screen_height != 0 || o.screen_height != 0 {
            return o.screen_height == 0;
        }
        if self.sdk_version != 0 || o.sdk_version != 0 {
            return o.sdk_version == 0;
        }
        if self.minor_version != 0 || o.minor_version != 0 {
            return o.minor_version == 0;
        }
        // Both configurations have nothing defined except some possible
        // future value. Returning the comparison of the two configurations
        // is a "best effort" at this point to protect against incorrect
        // dominations.
        self.compare(o) != Ordering::Equal
    }

    /// A configuration conflicts with another if both define an incompatible
    /// (non-range, non-density) parameter with different non-default values.
    /// Port of `ConfigDescription::ConflictsWith`.
    pub fn conflicts_with(&self, o: &ConfigDescription) -> bool {
        // This method should be updated as new configuration parameters are
        // introduced (e.g. screenConfig2).
        fn pred(a: u32, b: u32) -> bool {
            a == 0 || b == 0 || a == b
        }
        // The values here can be found in ResTable_config::match. Density
        // and range values can't lead to conflicts and are ignored.
        !pred(self.mcc as u32, o.mcc as u32)
            || !pred(self.mnc as u32, o.mnc as u32)
            || !pred(self.locale_u32(), o.locale_u32())
            || !pred(
                self.grammatical_inflection as u32,
                o.grammatical_inflection as u32,
            )
            || !pred(
                (self.screen_layout & Self::MASK_LAYOUTDIR) as u32,
                (o.screen_layout & Self::MASK_LAYOUTDIR) as u32,
            )
            || !pred(
                (self.screen_layout & Self::MASK_SCREENLONG) as u32,
                (o.screen_layout & Self::MASK_SCREENLONG) as u32,
            )
            || !pred(
                (self.ui_mode & Self::MASK_UI_MODE_TYPE) as u32,
                (o.ui_mode & Self::MASK_UI_MODE_TYPE) as u32,
            )
            || !pred(
                (self.ui_mode & Self::MASK_UI_MODE_NIGHT) as u32,
                (o.ui_mode & Self::MASK_UI_MODE_NIGHT) as u32,
            )
            || !pred(
                (self.screen_layout2 & Self::MASK_SCREENROUND) as u32,
                (o.screen_layout2 & Self::MASK_SCREENROUND) as u32,
            )
            || !pred(
                (self.color_mode & Self::MASK_HDR) as u32,
                (o.color_mode & Self::MASK_HDR) as u32,
            )
            || !pred(
                (self.color_mode & Self::MASK_WIDE_COLOR_GAMUT) as u32,
                (o.color_mode & Self::MASK_WIDE_COLOR_GAMUT) as u32,
            )
            || !pred(self.orientation as u32, o.orientation as u32)
            || !pred(self.touchscreen as u32, o.touchscreen as u32)
            || !pred(
                (self.input_flags & Self::MASK_KEYSHIDDEN) as u32,
                (o.input_flags & Self::MASK_KEYSHIDDEN) as u32,
            )
            || !pred(
                (self.input_flags & Self::MASK_NAVHIDDEN) as u32,
                (o.input_flags & Self::MASK_NAVHIDDEN) as u32,
            )
            || !pred(self.keyboard as u32, o.keyboard as u32)
            || !pred(self.navigation as u32, o.navigation as u32)
    }

    /// Two configurations are compatible if they can both match a common
    /// concrete device configuration and are unrelated by domination.
    pub fn is_compatible_with(&self, o: &ConfigDescription) -> bool {
        !self.conflicts_with(o) && !self.dominates(o) && !o.dominates(self)
    }

    // -----------------------------------------------------------------------
    // Locale string forms
    // -----------------------------------------------------------------------

    /// Appends the resource-qualifier representation of the locale to `out`:
    /// `en-rUS` for plain language/region, or the modified BCP-47 form
    /// `b+sr+Latn+RS` when a script/variant/numbering system is present.
    /// Port of `ResTable_config::appendDirLocale`.
    pub fn append_dir_locale(&self, out: &mut String) {
        if self.language[0] == 0 {
            return;
        }
        let script_was_provided = self.locale_script[0] != 0 && !self.locale_script_was_computed;
        if !script_was_provided
            && self.locale_variant[0] == 0
            && self.locale_numbering_system[0] == 0
        {
            // Legacy format.
            if !out.is_empty() {
                out.push('-');
            }
            out.push_str(&self.unpack_language());
            if self.country[0] != 0 {
                out.push_str("-r");
                out.push_str(&self.unpack_region());
            }
            return;
        }

        // We are writing the modified BCP 47 tag: it starts with 'b+' and
        // uses '+' as a separator.
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str("b+");
        out.push_str(&self.unpack_language());

        if script_was_provided {
            out.push('+');
            out.push_str(&bytes_to_string(&self.locale_script));
        }

        if self.country[0] != 0 {
            out.push('+');
            out.push_str(&self.unpack_region());
        }

        if self.locale_variant[0] != 0 {
            out.push('+');
            out.push_str(&bytes_to_string(&self.locale_variant));
        }

        if self.locale_numbering_system[0] != 0 {
            out.push_str("+u+nu+");
            out.push_str(&bytes_to_string(&self.locale_numbering_system));
        }
    }

    /// The BCP-47 language tag of this configuration's locale, e.g. `en-US`,
    /// `en-Latn-US`, `en-US-POSIX`, possibly with a `-u-nu-…` extension.
    /// If `canonicalize` is set, Tagalog (`tl`) becomes Filipino (`fil`).
    /// Port of `ResTable_config::getBcp47Locale`.
    pub fn get_bcp47_locale(&self, canonicalize: bool) -> String {
        let mut s = String::new();

        // The "any" locale is traditionally represented by the empty string.
        if self.language[0] == 0 && self.country[0] == 0 {
            return s;
        }

        if self.language[0] != 0 {
            if canonicalize && are_identical(&self.language, &K_TAGALOG) {
                // Replace Tagalog with Filipino if we are canonicalizing.
                s.push_str("fil");
            } else {
                s.push_str(&self.unpack_language());
            }
        }

        if self.locale_script[0] != 0 && !self.locale_script_was_computed {
            if !s.is_empty() {
                s.push('-');
            }
            s.push_str(&bytes_to_string(&self.locale_script));
        }

        if self.country[0] != 0 {
            if !s.is_empty() {
                s.push('-');
            }
            s.push_str(&self.unpack_region());
        }

        if self.locale_variant[0] != 0 {
            if !s.is_empty() {
                s.push('-');
            }
            s.push_str(&bytes_to_string(&self.locale_variant));
        }

        // Add the Unicode extension only if at least one other locale
        // component is present.
        if self.locale_numbering_system[0] != 0 && !s.is_empty() {
            s.push_str("-u-nu-");
            s.push_str(&bytes_to_string(&self.locale_numbering_system));
        }

        s
    }

    /// Sets language/region/script/variant/numbering system from a
    /// well-formed BCP-47 locale tag (no validation is performed). Port of
    /// `ResTable_config::setBcp47Locale`.
    pub fn set_bcp47_locale(&mut self, input: &str) {
        self.clear_locale();

        let mut state = LocaleParserState::default();
        for subtag in input.split('-') {
            assign_locale_component(self, subtag, &mut state);
            if state.parser == ParserState::IgnoreTheRest {
                break;
            }
        }

        self.locale_script_was_computed = self.locale_script[0] == 0;
        if self.locale_script_was_computed {
            self.compute_script();
        }
    }
}

// ---------------------------------------------------------------------------
// Ordering / Display
// ---------------------------------------------------------------------------

impl PartialOrd for ConfigDescription {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.compare(other))
    }
}

impl Ord for ConfigDescription {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare(other)
    }
}

impl fmt::Display for ConfigDescription {
    /// Port of `ResTable_config::toString()`: the qualifier string used by
    /// `aapt2 dump` and directory naming. The default config prints as `""`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut res = String::new();

        if self.mcc != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("mcc{}", self.mcc));
        }
        if self.mnc != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("mnc{}", self.mnc));
        }

        self.append_dir_locale(&mut res);

        if (self.grammatical_inflection & Self::GRAMMATICAL_INFLECTION_GENDER_MASK) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.grammatical_inflection & Self::GRAMMATICAL_INFLECTION_GENDER_MASK {
                Self::GRAMMATICAL_GENDER_NEUTER => res.push_str("neuter"),
                Self::GRAMMATICAL_GENDER_FEMININE => res.push_str("feminine"),
                Self::GRAMMATICAL_GENDER_MASCULINE => res.push_str("masculine"),
                _ => {}
            }
        }

        if (self.screen_layout & Self::MASK_LAYOUTDIR) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.screen_layout & Self::MASK_LAYOUTDIR {
                Self::LAYOUTDIR_LTR => res.push_str("ldltr"),
                Self::LAYOUTDIR_RTL => res.push_str("ldrtl"),
                other => res.push_str(&format!("layoutDir={other}")),
            }
        }
        if self.smallest_screen_width_dp != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("sw{}dp", self.smallest_screen_width_dp));
        }
        if self.screen_width_dp != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("w{}dp", self.screen_width_dp));
        }
        if self.screen_height_dp != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("h{}dp", self.screen_height_dp));
        }
        if (self.screen_layout & Self::MASK_SCREENSIZE) != Self::SCREENSIZE_ANY {
            if !res.is_empty() {
                res.push('-');
            }
            match self.screen_layout & Self::MASK_SCREENSIZE {
                Self::SCREENSIZE_SMALL => res.push_str("small"),
                Self::SCREENSIZE_NORMAL => res.push_str("normal"),
                Self::SCREENSIZE_LARGE => res.push_str("large"),
                Self::SCREENSIZE_XLARGE => res.push_str("xlarge"),
                other => res.push_str(&format!("screenLayoutSize={other}")),
            }
        }
        if (self.screen_layout & Self::MASK_SCREENLONG) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.screen_layout & Self::MASK_SCREENLONG {
                Self::SCREENLONG_NO => res.push_str("notlong"),
                Self::SCREENLONG_YES => res.push_str("long"),
                other => res.push_str(&format!("screenLayoutLong={other}")),
            }
        }
        if (self.screen_layout2 & Self::MASK_SCREENROUND) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.screen_layout2 & Self::MASK_SCREENROUND {
                Self::SCREENROUND_NO => res.push_str("notround"),
                Self::SCREENROUND_YES => res.push_str("round"),
                other => res.push_str(&format!("screenRound={other}")),
            }
        }
        if (self.color_mode & Self::MASK_WIDE_COLOR_GAMUT) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.color_mode & Self::MASK_WIDE_COLOR_GAMUT {
                Self::WIDE_COLOR_GAMUT_NO => res.push_str("nowidecg"),
                Self::WIDE_COLOR_GAMUT_YES => res.push_str("widecg"),
                other => res.push_str(&format!("wideColorGamut={other}")),
            }
        }
        if (self.color_mode & Self::MASK_HDR) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.color_mode & Self::MASK_HDR {
                Self::HDR_NO => res.push_str("lowdr"),
                Self::HDR_YES => res.push_str("highdr"),
                other => res.push_str(&format!("hdr={other}")),
            }
        }
        if self.orientation != Self::ORIENTATION_ANY {
            if !res.is_empty() {
                res.push('-');
            }
            match self.orientation {
                Self::ORIENTATION_PORT => res.push_str("port"),
                Self::ORIENTATION_LAND => res.push_str("land"),
                Self::ORIENTATION_SQUARE => res.push_str("square"),
                other => res.push_str(&format!("orientation={other}")),
            }
        }
        if (self.ui_mode & Self::MASK_UI_MODE_TYPE) != Self::UI_MODE_TYPE_ANY {
            if !res.is_empty() {
                res.push('-');
            }
            match self.ui_mode & Self::MASK_UI_MODE_TYPE {
                Self::UI_MODE_TYPE_DESK => res.push_str("desk"),
                Self::UI_MODE_TYPE_CAR => res.push_str("car"),
                Self::UI_MODE_TYPE_TELEVISION => res.push_str("television"),
                Self::UI_MODE_TYPE_APPLIANCE => res.push_str("appliance"),
                Self::UI_MODE_TYPE_WATCH => res.push_str("watch"),
                Self::UI_MODE_TYPE_VR_HEADSET => res.push_str("vrheadset"),
                _ => {
                    // NOTE: the C++ prints screenLayout here (an upstream
                    // bug, ported faithfully).
                    res.push_str(&format!(
                        "uiModeType={}",
                        self.screen_layout & Self::MASK_UI_MODE_TYPE
                    ));
                }
            }
        }
        if (self.ui_mode & Self::MASK_UI_MODE_NIGHT) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.ui_mode & Self::MASK_UI_MODE_NIGHT {
                Self::UI_MODE_NIGHT_NO => res.push_str("notnight"),
                Self::UI_MODE_NIGHT_YES => res.push_str("night"),
                other => res.push_str(&format!("uiModeNight={other}")),
            }
        }
        if self.density != Self::DENSITY_DEFAULT {
            if !res.is_empty() {
                res.push('-');
            }
            match self.density {
                Self::DENSITY_LOW => res.push_str("ldpi"),
                Self::DENSITY_MEDIUM => res.push_str("mdpi"),
                Self::DENSITY_TV => res.push_str("tvdpi"),
                Self::DENSITY_HIGH => res.push_str("hdpi"),
                Self::DENSITY_XHIGH => res.push_str("xhdpi"),
                Self::DENSITY_XXHIGH => res.push_str("xxhdpi"),
                Self::DENSITY_XXXHIGH => res.push_str("xxxhdpi"),
                Self::DENSITY_NONE => res.push_str("nodpi"),
                Self::DENSITY_ANY => res.push_str("anydpi"),
                other => res.push_str(&format!("{other}dpi")),
            }
        }
        if self.touchscreen != Self::TOUCHSCREEN_ANY {
            if !res.is_empty() {
                res.push('-');
            }
            match self.touchscreen {
                Self::TOUCHSCREEN_NOTOUCH => res.push_str("notouch"),
                Self::TOUCHSCREEN_FINGER => res.push_str("finger"),
                Self::TOUCHSCREEN_STYLUS => res.push_str("stylus"),
                other => res.push_str(&format!("touchscreen={other}")),
            }
        }
        if (self.input_flags & Self::MASK_KEYSHIDDEN) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.input_flags & Self::MASK_KEYSHIDDEN {
                Self::KEYSHIDDEN_NO => res.push_str("keysexposed"),
                Self::KEYSHIDDEN_YES => res.push_str("keyshidden"),
                Self::KEYSHIDDEN_SOFT => res.push_str("keyssoft"),
                _ => {}
            }
        }
        if self.keyboard != Self::KEYBOARD_ANY {
            if !res.is_empty() {
                res.push('-');
            }
            match self.keyboard {
                Self::KEYBOARD_NOKEYS => res.push_str("nokeys"),
                Self::KEYBOARD_QWERTY => res.push_str("qwerty"),
                Self::KEYBOARD_12KEY => res.push_str("12key"),
                other => res.push_str(&format!("keyboard={other}")),
            }
        }
        if (self.input_flags & Self::MASK_NAVHIDDEN) != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            match self.input_flags & Self::MASK_NAVHIDDEN {
                Self::NAVHIDDEN_NO => res.push_str("navexposed"),
                Self::NAVHIDDEN_YES => res.push_str("navhidden"),
                other => res.push_str(&format!("inputFlagsNavHidden={other}")),
            }
        }
        if self.navigation != Self::NAVIGATION_ANY {
            if !res.is_empty() {
                res.push('-');
            }
            match self.navigation {
                Self::NAVIGATION_NONAV => res.push_str("nonav"),
                Self::NAVIGATION_DPAD => res.push_str("dpad"),
                Self::NAVIGATION_TRACKBALL => res.push_str("trackball"),
                Self::NAVIGATION_WHEEL => res.push_str("wheel"),
                other => res.push_str(&format!("navigation={other}")),
            }
        }
        if self.screen_size_u32() != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("{}x{}", self.screen_width, self.screen_height));
        }
        if self.version_u32() != 0 {
            if !res.is_empty() {
                res.push('-');
            }
            res.push_str(&format!("v{}", self.sdk_version));
            if self.minor_version != 0 {
                res.push_str(&format!(".{}", self.minor_version));
            }
        }

        f.write_str(&res)
    }
}

// ---------------------------------------------------------------------------
// Packed language/region codes (ResourceTypes.cpp pack/unpackLanguageOrRegion)
// ---------------------------------------------------------------------------

/// Packs an up-to-3-character code. Two-letter codes are stored verbatim;
/// three-letter codes use the base-relative 5-bit-per-letter big-endian
/// packing with the high bit of byte 0 set.
fn pack_language_or_region(input: &[u8], base: u8) -> [u8; 2] {
    let c0 = input.first().copied().unwrap_or(0);
    let c1 = input.get(1).copied().unwrap_or(0);
    let c2 = input.get(2).copied().unwrap_or(0);
    if c2 == 0 || c2 == b'-' {
        [c0, c1]
    } else {
        let first = (c0.wrapping_sub(base) & 0x7f) as u32;
        let second = (c1.wrapping_sub(base) & 0x7f) as u32;
        let third = (c2.wrapping_sub(base) & 0x7f) as u32;
        [
            (0x80 | (third << 2) | (second >> 3)) as u8,
            ((second << 5) | first) as u8,
        ]
    }
}

/// Unpacks a 2-byte packed code into its 0-, 2-, or 3-character string form.
fn unpack_language_or_region(input: &[u8; 2], base: u8) -> String {
    if input[0] & 0x80 != 0 {
        // The high bit is "1": a packed three-letter code.
        let first = input[1] & 0x1f;
        let second = ((input[1] & 0xe0) >> 5) + ((input[0] & 0x03) << 3);
        let third = (input[0] & 0x7c) >> 2;
        bytes_to_string(&[
            first.wrapping_add(base),
            second.wrapping_add(base),
            third.wrapping_add(base),
        ])
    } else if input[0] != 0 {
        bytes_to_string(&input[..])
    } else {
        String::new()
    }
}

/// ASCII bytes (up to the first NUL) as a `String`.
fn bytes_to_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

// ---------------------------------------------------------------------------
// Locale comparison helpers (ResourceTypes.cpp)
// ---------------------------------------------------------------------------

// Codes for specially handled languages and regions.
const K_ENGLISH: [u8; 2] = [b'e', b'n'];
const K_UNITED_STATES: [u8; 2] = [b'U', b'S'];
/// Packed version of "fil".
const K_FILIPINO: [u8; 2] = [0xAD, 0x05];
const K_TAGALOG: [u8; 2] = [b't', b'l'];

/// Checks if two language or region codes are identical.
fn are_identical(code1: &[u8; 2], code2: &[u8; 2]) -> bool {
    code1 == code2
}

fn langs_are_equivalent(lang1: &[u8; 2], lang2: &[u8; 2]) -> bool {
    are_identical(lang1, lang2)
        || (are_identical(lang1, &K_TAGALOG) && are_identical(lang2, &K_FILIPINO))
        || (are_identical(lang1, &K_FILIPINO) && are_identical(lang2, &K_TAGALOG))
}

/// `compareLocales`: orders by the packed locale word, then the (masked)
/// script, variant, and numbering system.
fn compare_locales(l: &ConfigDescription, r: &ConfigDescription) -> Ordering {
    let lv = l.locale_u32();
    let rv = r.locale_u32();
    if lv != rv {
        return lv.cmp(&rv);
    }

    // Languages and regions are equal: compare scripts, variants, and
    // numbering systems, masking out computed scripts.
    const EMPTY_SCRIPT: [u8; 4] = [0; 4];
    let l_script = if l.locale_script_was_computed {
        &EMPTY_SCRIPT
    } else {
        &l.locale_script
    };
    let r_script = if r.locale_script_was_computed {
        &EMPTY_SCRIPT
    } else {
        &r.locale_script
    };
    let script = l_script.cmp(r_script);
    if script != Ordering::Equal {
        return script;
    }

    let variant = l.locale_variant.cmp(&r.locale_variant);
    if variant != Ordering::Equal {
        return variant;
    }

    l.locale_numbering_system.cmp(&r.locale_numbering_system)
}

// ---------------------------------------------------------------------------
// LocaleData (LocaleData.cpp, algorithm only — see module docs)
// ---------------------------------------------------------------------------

const PACKED_ROOT: u32 = 0; // Represents the root locale.

/// Packs a (language, region) pair of 2-byte fields into a big-endian-style
/// `u32` key (`'e','s','U','S'` → `0x65735553`).
fn pack_locale(language: &[u8; 2], region: &[u8; 2]) -> u32 {
    (language[0] as u32) << 24
        | (language[1] as u32) << 16
        | (region[0] as u32) << 8
        | region[1] as u32
}

fn drop_region(packed_locale: u32) -> u32 {
    packed_locale & 0xFFFF_0000
}

fn has_region(packed_locale: u32) -> bool {
    packed_locale & 0x0000_FFFF != 0
}

/// Without the CLDR parent table, the only parent of `lang-REGION` is
/// `lang`, and the parent of `lang` is the root.
fn find_parent(packed_locale: u32) -> u32 {
    if has_region(packed_locale) {
        drop_region(packed_locale)
    } else {
        PACKED_ROOT
    }
}

/// Walks the ancestor chain of `packed_locale`, optionally recording it in
/// `out`. Stops early when a member of `stop_list` is seen, returning its
/// index; otherwise returns -1. The returned count includes the stopping
/// ancestor.
fn find_ancestors(
    mut out: Option<&mut Vec<u32>>,
    packed_locale: u32,
    stop_list: &[u32],
) -> (usize, isize) {
    let mut ancestor = packed_locale;
    let mut count = 0usize;
    loop {
        if let Some(v) = out.as_deref_mut() {
            v.push(ancestor);
        }
        count += 1;
        if let Some(i) = stop_list.iter().position(|&s| s == ancestor) {
            return (count, i as isize);
        }
        ancestor = find_parent(ancestor);
        if ancestor == PACKED_ROOT {
            return (count, -1);
        }
    }
}

/// Distance in the parent tree between `supported` and the request, via
/// their lowest common ancestor.
fn find_distance(supported: u32, request_ancestors: &[u32]) -> isize {
    let (supported_ancestor_count, request_ancestors_index) =
        find_ancestors(None, supported, request_ancestors);
    supported_ancestor_count as isize + request_ancestors_index - 1
}

/// Whether the locale is the representative of its language (table lookup
/// in C++; always false without the CLDR tables).
fn is_locale_representative(_language_and_region: u32) -> bool {
    false
}

const US_SPANISH: u32 = 0x6573_5553; // es-US
const MEXICAN_SPANISH: u32 = 0x6573_4D58; // es-MX
const LATIN_AMERICAN_SPANISH: u32 = 0x6573_A424; // es-419

/// es-US and es-MX are special fallbacks for es-419: if there is no es-419
/// they are considered its equivalent.
fn is_special_spanish(language_and_region: u32) -> bool {
    language_and_region == US_SPANISH || language_and_region == MEXICAN_SPANISH
}

/// Port of `localeDataCompareRegions`: positive if `left_region` is a better
/// match for the requested locale, negative if `right_region` is, 0 if tied.
fn locale_data_compare_regions(
    left_region: &[u8; 2],
    right_region: &[u8; 2],
    requested_language: &[u8; 2],
    _requested_script: &[u8; 4],
    requested_region: &[u8; 2],
) -> i32 {
    if left_region == right_region {
        return 0;
    }
    let mut left = pack_locale(requested_language, left_region);
    let mut right = pack_locale(requested_language, right_region);
    let request = pack_locale(requested_language, requested_region);

    // If one and only one of the two locales is a special Spanish locale,
    // replace it with es-419 (unless the other is already es-419, or both
    // are special Spanish).
    let left_is_special_spanish = is_special_spanish(left);
    let right_is_special_spanish = is_special_spanish(right);
    if left_is_special_spanish && !right_is_special_spanish && right != LATIN_AMERICAN_SPANISH {
        left = LATIN_AMERICAN_SPANISH;
    } else if right_is_special_spanish && !left_is_special_spanish && left != LATIN_AMERICAN_SPANISH
    {
        right = LATIN_AMERICAN_SPANISH;
    }

    // Find the parents of the request, but stop as soon as we see left or
    // right.
    let mut request_ancestors = Vec::new();
    let left_and_right = [left, right];
    let (_, left_right_index) =
        find_ancestors(Some(&mut request_ancestors), request, &left_and_right);
    if left_right_index == 0 {
        // We saw left earlier.
        return 1;
    }
    if left_right_index == 1 {
        // We saw right earlier.
        return -1;
    }

    // Neither left nor right is an ancestor of the request: use the distance
    // in the parent tree.
    let left_distance = find_distance(left, &request_ancestors);
    let right_distance = find_distance(right, &request_ancestors);
    if left_distance != right_distance {
        return (right_distance - left_distance) as i32; // Smaller distance is better.
    }

    // Equidistant: prefer a representative locale (never true without the
    // CLDR tables).
    let left_is_representative = is_locale_representative(left);
    let right_is_representative = is_locale_representative(right);
    if left_is_representative != right_is_representative {
        return left_is_representative as i32 - right_is_representative as i32;
    }

    // No way to figure out which is better; for stability prefer the lower
    // region code (two-letter codes order before three-digit codes).
    ((right as i64) - (left as i64)).signum() as i32
}

const PACKED_EN: u32 = 0x656E_0000; // en
const PACKED_EN_001: u32 = 0x656E_8400; // en-001

/// A locale is "close to US English" if `en` is seen before `en-001` in its
/// ancestor list.
fn locale_data_is_close_to_us_english(region: &[u8; 2]) -> bool {
    let locale = pack_locale(&K_ENGLISH, region);
    let (_, stop_list_index) = find_ancestors(None, locale, &[PACKED_EN, PACKED_EN_001]);
    stop_list_index == 0
}

/// Likely-script lookup. Always unknown without the CLDR tables (see module
/// docs); the C++ also returns empty for locales missing from the table.
fn locale_data_compute_script(_language: &[u8; 2], _region: &[u8; 2]) -> [u8; 4] {
    [0; 4]
}

// ---------------------------------------------------------------------------
// setBcp47Locale parser state machine (ResourceTypes.cpp LocaleParserState)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    Base,
    UnicodeExtension,
    IgnoreTheRest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnicodeState {
    /// Initial state after the Unicode singleton is detected. Either a
    /// keyword or an attribute is expected.
    NoKey,
    /// Unicode extension key (but not attribute) is expected.
    ExpectKey,
    /// A key was detected but is unsupported; ignore its value.
    IgnoreKey,
    /// Numbering system key was detected; store its value.
    NumberingSystem,
}

#[derive(Debug, Clone, Copy)]
struct LocaleParserState {
    parser: ParserState,
    unicode: UnicodeState,
}

impl Default for LocaleParserState {
    fn default() -> Self {
        LocaleParserState {
            parser: ParserState::Base,
            unicode: UnicodeState::NoKey,
        }
    }
}

/// Port of `assignLocaleComponent`.
fn assign_locale_component(
    config: &mut ConfigDescription,
    part: &str,
    state: &mut LocaleParserState,
) {
    let bytes = part.as_bytes();
    let size = bytes.len();

    if state.parser == ParserState::UnicodeExtension {
        match size {
            1 => {
                // Other BCP 47 extensions are not supported at the moment.
                state.parser = ParserState::IgnoreTheRest;
            }
            2 => {
                if state.unicode == UnicodeState::NoKey || state.unicode == UnicodeState::ExpectKey
                {
                    // Currently only 'nu' (numbering system) is supported.
                    if bytes[0].eq_ignore_ascii_case(&b'n') && bytes[1].eq_ignore_ascii_case(&b'u')
                    {
                        state.unicode = UnicodeState::NumberingSystem;
                    } else {
                        state.unicode = UnicodeState::IgnoreKey;
                    }
                } else {
                    // Keys are not allowed in other states; ignore the rest.
                    state.parser = ParserState::IgnoreTheRest;
                }
            }
            3..=8 => match state.unicode {
                UnicodeState::NumberingSystem => {
                    // Accept only the first occurrence of the numbering system.
                    if config.locale_numbering_system[0] == 0 {
                        for (i, &b) in bytes.iter().enumerate() {
                            config.locale_numbering_system[i] = b.to_ascii_lowercase();
                        }
                        state.unicode = UnicodeState::ExpectKey;
                    } else {
                        state.parser = ParserState::IgnoreTheRest;
                    }
                }
                UnicodeState::IgnoreKey => {
                    // Unsupported Unicode keyword; ignore.
                    state.unicode = UnicodeState::ExpectKey;
                }
                UnicodeState::ExpectKey => {
                    // A keyword followed by an attribute is not allowed.
                    state.parser = ParserState::IgnoreTheRest;
                }
                UnicodeState::NoKey => {
                    // Extension attribute; do nothing.
                }
            },
            _ => {
                // Unexpected field length; treat as an error.
                state.parser = ParserState::IgnoreTheRest;
            }
        }
        return;
    }

    match size {
        0 => state.parser = ParserState::IgnoreTheRest,
        1 => {
            state.parser = if bytes[0].eq_ignore_ascii_case(&b'u') {
                ParserState::UnicodeExtension
            } else {
                ParserState::IgnoreTheRest
            };
        }
        2 | 3 => {
            if config.language[0] != 0 {
                config.pack_region(bytes);
            } else {
                config.pack_language(bytes);
            }
        }
        4 if !bytes[0].is_ascii_digit() => {
            config.locale_script[0] = bytes[0].to_ascii_uppercase();
            for i in 1..4 {
                config.locale_script[i] = bytes[i].to_ascii_lowercase();
            }
        }
        4..=8 => {
            // A variant (4-char variants start with a digit).
            config.locale_variant = [0; 8];
            for (i, &b) in bytes.iter().enumerate() {
                config.locale_variant[i] = b.to_ascii_lowercase();
            }
        }
        _ => state.parser = ParserState::IgnoreTheRest,
    }
}

// ---------------------------------------------------------------------------
// LocaleValue (Locale.h / Locale.cpp)
// ---------------------------------------------------------------------------

/// A convenience type to build and parse locales. Mirrors
/// `android::LocaleValue` (all fields zero-filled fixed buffers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LocaleValue {
    pub language: [u8; 4],
    pub region: [u8; 4],
    pub script: [u8; 4],
    pub variant: [u8; 8],
}

fn is_alpha(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphabetic())
}

impl LocaleValue {
    fn set_language(&mut self, language: &[u8]) {
        self.language = [0; 4];
        for (i, &b) in language.iter().take(4).enumerate() {
            self.language[i] = b.to_ascii_lowercase();
        }
    }

    fn set_region(&mut self, region: &[u8]) {
        self.region = [0; 4];
        for (i, &b) in region.iter().take(4).enumerate() {
            self.region[i] = b.to_ascii_uppercase();
        }
    }

    fn set_script(&mut self, script: &[u8]) {
        self.script = [0; 4];
        for (i, &b) in script.iter().take(4).enumerate() {
            self.script[i] = if i == 0 {
                b.to_ascii_uppercase()
            } else {
                b.to_ascii_lowercase()
            };
        }
    }

    fn set_variant(&mut self, variant: &[u8]) {
        self.variant = [0; 8];
        for (i, &b) in variant.iter().take(8).enumerate() {
            self.variant[i] = b;
        }
    }

    /// Initializes from a BCP-47 locale tag (`-`-separated).
    pub fn init_from_bcp47_tag(tag: &str) -> Option<LocaleValue> {
        let mut lv = LocaleValue::default();
        if lv.init_from_bcp47_tag_impl(tag, '-') {
            Some(lv)
        } else {
            None
        }
    }

    /// Port of `LocaleValue::InitFromBcp47TagImpl`.
    fn init_from_bcp47_tag_impl(&mut self, bcp47tag: &str, separator: char) -> bool {
        let subtags: Vec<String> = bcp47tag
            .split(separator)
            .map(|s| s.to_ascii_lowercase())
            .collect();
        match subtags.len() {
            1 => self.set_language(subtags[0].as_bytes()),
            2 => {
                self.set_language(subtags[0].as_bytes());
                // The second tag can either be a region, a variant or a script.
                match subtags[1].len() {
                    2 | 3 => self.set_region(subtags[1].as_bytes()),
                    4 if !subtags[1].as_bytes()[0].is_ascii_digit() => {
                        self.set_script(subtags[1].as_bytes());
                    }
                    4..=8 => self.set_variant(subtags[1].as_bytes()),
                    _ => return false,
                }
            }
            3 => {
                // The language is always the first subtag.
                self.set_language(subtags[0].as_bytes());

                // The second subtag is a script if its size is 4, else a
                // region code.
                if subtags[1].len() == 4 {
                    self.set_script(subtags[1].as_bytes());
                } else if subtags[1].len() == 2 || subtags[1].len() == 3 {
                    self.set_region(subtags[1].as_bytes());
                } else {
                    return false;
                }

                // The third tag is a region code if the second was a script,
                // else a variant code.
                if subtags[2].len() >= 4 {
                    self.set_variant(subtags[2].as_bytes());
                } else {
                    self.set_region(subtags[2].as_bytes());
                }
            }
            4 => {
                self.set_language(subtags[0].as_bytes());
                self.set_script(subtags[1].as_bytes());
                self.set_region(subtags[2].as_bytes());
                self.set_variant(subtags[3].as_bytes());
            }
            _ => return false,
        }
        true
    }

    /// Port of `LocaleValue::InitFromParts`: consumes leading locale parts
    /// (already lowercased) from a qualifier-string part list. Returns the
    /// parsed value and how many parts were consumed (0 is valid: no
    /// locale), or `None` for a malformed `b+…` tag.
    pub fn init_from_parts(parts: &[String]) -> Option<(LocaleValue, usize)> {
        let mut lv = LocaleValue::default();
        let mut consumed = 0usize;

        let part = &parts[0];
        if part.len() >= 2 && part.starts_with("b+") {
            // A "modified" BCP 47 language tag: same semantics, but the
            // separator is '+'. Skip the prefix.
            if !lv.init_from_bcp47_tag_impl(&part[2..], '+') {
                return None;
            }
            consumed = 1;
        } else if (part.len() == 2 || part.len() == 3) && is_alpha(part) && part != "car" {
            lv.set_language(part.as_bytes());
            consumed = 1;

            if consumed < parts.len() {
                let region_part = parts[consumed].as_bytes();
                if !region_part.is_empty() && region_part[0] == b'r' && region_part.len() == 3 {
                    lv.set_region(&region_part[1..]);
                    consumed += 1;
                }
            }
        }
        Some((lv, consumed))
    }

    /// Port of `LocaleValue::WriteTo`: stores this locale into a config.
    pub fn write_to(&self, out: &mut ConfigDescription) {
        out.pack_language(&self.language);
        out.pack_region(&self.region);

        if self.script[0] != 0 {
            out.locale_script = self.script;
        }

        if self.variant[0] != 0 {
            out.locale_variant = self.variant;
        }
    }
}

// ---------------------------------------------------------------------------
// Qualifier part parsers (ConfigDescription.cpp parse*)
// ---------------------------------------------------------------------------

const WILDCARD_NAME: &str = "any";

/// atoi-then-truncate-to-u16, for all-digit inputs (mirrors `(uint16_t)atoi`).
fn atoi_u16(s: &str) -> u16 {
    s.parse::<u64>().map(|v| v as u16).unwrap_or(0)
}

/// Splits a leading run of ASCII digits from the rest.
fn split_digits_prefix(s: &str) -> (&str, &str) {
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    s.split_at(end)
}

fn parse_mcc(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.mcc = 0;
        return true;
    }
    let val = match name.strip_prefix("mcc") {
        Some(v) => v,
        None => return false,
    };
    let (digits, rest) = split_digits_prefix(val);
    if !rest.is_empty() || digits.len() != 3 {
        return false;
    }
    let d = atoi_u16(digits);
    if d != 0 {
        out.mcc = d;
        return true;
    }
    false
}

fn parse_mnc(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.mnc = 0;
        return true;
    }
    let val = match name.strip_prefix("mnc") {
        Some(v) => v,
        None => return false,
    };
    let (digits, rest) = split_digits_prefix(val);
    if !rest.is_empty() || digits.is_empty() || digits.len() > 3 {
        return false;
    }
    out.mnc = atoi_u16(digits);
    if out.mnc == 0 {
        out.mnc = MNC_ZERO;
    }
    true
}

fn parse_grammatical_inflection(name: &str, out: &mut ConfigDescription) -> bool {
    match name {
        "feminine" => {
            out.grammatical_inflection = ConfigDescription::GRAMMATICAL_GENDER_FEMININE;
            true
        }
        "masculine" => {
            out.grammatical_inflection = ConfigDescription::GRAMMATICAL_GENDER_MASCULINE;
            true
        }
        "neuter" => {
            out.grammatical_inflection = ConfigDescription::GRAMMATICAL_GENDER_NEUTER;
            true
        }
        _ => false,
    }
}

fn parse_layout_direction(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::LAYOUTDIR_ANY,
        "ldltr" => ConfigDescription::LAYOUTDIR_LTR,
        "ldrtl" => ConfigDescription::LAYOUTDIR_RTL,
        _ => return false,
    };
    out.screen_layout = (out.screen_layout & !ConfigDescription::MASK_LAYOUTDIR) | value;
    true
}

fn parse_screen_layout_size(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::SCREENSIZE_ANY,
        "small" => ConfigDescription::SCREENSIZE_SMALL,
        "normal" => ConfigDescription::SCREENSIZE_NORMAL,
        "large" => ConfigDescription::SCREENSIZE_LARGE,
        "xlarge" => ConfigDescription::SCREENSIZE_XLARGE,
        _ => return false,
    };
    out.screen_layout = (out.screen_layout & !ConfigDescription::MASK_SCREENSIZE) | value;
    true
}

fn parse_screen_layout_long(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::SCREENLONG_ANY,
        "long" => ConfigDescription::SCREENLONG_YES,
        "notlong" => ConfigDescription::SCREENLONG_NO,
        _ => return false,
    };
    out.screen_layout = (out.screen_layout & !ConfigDescription::MASK_SCREENLONG) | value;
    true
}

fn parse_screen_round(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::SCREENROUND_ANY,
        "round" => ConfigDescription::SCREENROUND_YES,
        "notround" => ConfigDescription::SCREENROUND_NO,
        _ => return false,
    };
    out.screen_layout2 = (out.screen_layout2 & !ConfigDescription::MASK_SCREENROUND) | value;
    true
}

fn parse_wide_color_gamut(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::WIDE_COLOR_GAMUT_ANY,
        "widecg" => ConfigDescription::WIDE_COLOR_GAMUT_YES,
        "nowidecg" => ConfigDescription::WIDE_COLOR_GAMUT_NO,
        _ => return false,
    };
    out.color_mode = (out.color_mode & !ConfigDescription::MASK_WIDE_COLOR_GAMUT) | value;
    true
}

fn parse_hdr(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::HDR_ANY,
        "highdr" => ConfigDescription::HDR_YES,
        "lowdr" => ConfigDescription::HDR_NO,
        _ => return false,
    };
    out.color_mode = (out.color_mode & !ConfigDescription::MASK_HDR) | value;
    true
}

fn parse_orientation(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::ORIENTATION_ANY,
        "port" => ConfigDescription::ORIENTATION_PORT,
        "land" => ConfigDescription::ORIENTATION_LAND,
        "square" => ConfigDescription::ORIENTATION_SQUARE,
        _ => return false,
    };
    out.orientation = value;
    true
}

fn parse_ui_mode_type(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::UI_MODE_TYPE_ANY,
        "desk" => ConfigDescription::UI_MODE_TYPE_DESK,
        "car" => ConfigDescription::UI_MODE_TYPE_CAR,
        "television" => ConfigDescription::UI_MODE_TYPE_TELEVISION,
        "appliance" => ConfigDescription::UI_MODE_TYPE_APPLIANCE,
        "watch" => ConfigDescription::UI_MODE_TYPE_WATCH,
        "vrheadset" => ConfigDescription::UI_MODE_TYPE_VR_HEADSET,
        _ => return false,
    };
    out.ui_mode = (out.ui_mode & !ConfigDescription::MASK_UI_MODE_TYPE) | value;
    true
}

fn parse_ui_mode_night(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::UI_MODE_NIGHT_ANY,
        "night" => ConfigDescription::UI_MODE_NIGHT_YES,
        "notnight" => ConfigDescription::UI_MODE_NIGHT_NO,
        _ => return false,
    };
    out.ui_mode = (out.ui_mode & !ConfigDescription::MASK_UI_MODE_NIGHT) | value;
    true
}

fn parse_density(name: &str, out: &mut ConfigDescription) -> bool {
    let named = match name {
        WILDCARD_NAME => Some(ConfigDescription::DENSITY_DEFAULT),
        "anydpi" => Some(ConfigDescription::DENSITY_ANY),
        "nodpi" => Some(ConfigDescription::DENSITY_NONE),
        "ldpi" => Some(ConfigDescription::DENSITY_LOW),
        "mdpi" => Some(ConfigDescription::DENSITY_MEDIUM),
        "tvdpi" => Some(ConfigDescription::DENSITY_TV),
        "hdpi" => Some(ConfigDescription::DENSITY_HIGH),
        "xhdpi" => Some(ConfigDescription::DENSITY_XHIGH),
        "xxhdpi" => Some(ConfigDescription::DENSITY_XXHIGH),
        "xxxhdpi" => Some(ConfigDescription::DENSITY_XXXHIGH),
        _ => None,
    };
    if let Some(d) = named {
        out.density = d;
        return true;
    }

    let (digits, rest) = split_digits_prefix(name);
    // Check that we have 'dpi' after the last digit.
    if !rest.eq_ignore_ascii_case("dpi") {
        return false;
    }
    let d = match digits.parse::<u64>() {
        Ok(d) => d,
        Err(_) => return false, // Empty digit run (or absurd overflow).
    };
    if d != 0 {
        out.density = d as u16;
        return true;
    }
    false
}

fn parse_touchscreen(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::TOUCHSCREEN_ANY,
        "notouch" => ConfigDescription::TOUCHSCREEN_NOTOUCH,
        "stylus" => ConfigDescription::TOUCHSCREEN_STYLUS,
        "finger" => ConfigDescription::TOUCHSCREEN_FINGER,
        _ => return false,
    };
    out.touchscreen = value;
    true
}

fn parse_keys_hidden(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::KEYSHIDDEN_ANY,
        "keysexposed" => ConfigDescription::KEYSHIDDEN_NO,
        "keyshidden" => ConfigDescription::KEYSHIDDEN_YES,
        "keyssoft" => ConfigDescription::KEYSHIDDEN_SOFT,
        _ => return false,
    };
    out.input_flags = (out.input_flags & !ConfigDescription::MASK_KEYSHIDDEN) | value;
    true
}

fn parse_keyboard(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::KEYBOARD_ANY,
        "nokeys" => ConfigDescription::KEYBOARD_NOKEYS,
        "qwerty" => ConfigDescription::KEYBOARD_QWERTY,
        "12key" => ConfigDescription::KEYBOARD_12KEY,
        _ => return false,
    };
    out.keyboard = value;
    true
}

fn parse_nav_hidden(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::NAVHIDDEN_ANY,
        "navexposed" => ConfigDescription::NAVHIDDEN_NO,
        "navhidden" => ConfigDescription::NAVHIDDEN_YES,
        _ => return false,
    };
    out.input_flags = (out.input_flags & !ConfigDescription::MASK_NAVHIDDEN) | value;
    true
}

fn parse_navigation(name: &str, out: &mut ConfigDescription) -> bool {
    let value = match name {
        WILDCARD_NAME => ConfigDescription::NAVIGATION_ANY,
        "nonav" => ConfigDescription::NAVIGATION_NONAV,
        "dpad" => ConfigDescription::NAVIGATION_DPAD,
        "trackball" => ConfigDescription::NAVIGATION_TRACKBALL,
        "wheel" => ConfigDescription::NAVIGATION_WHEEL,
        _ => return false,
    };
    out.navigation = value;
    true
}

fn parse_screen_size(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.screen_width = ConfigDescription::SCREENWIDTH_ANY;
        out.screen_height = ConfigDescription::SCREENHEIGHT_ANY;
        return true;
    }

    let (w_digits, rest) = split_digits_prefix(name);
    if w_digits.is_empty() || !rest.starts_with('x') {
        return false;
    }
    let h_part = &rest[1..];
    let (h_digits, h_rest) = split_digits_prefix(h_part);
    // NOTE: the C++ checks `y == name` (never true after a found 'x'), so an
    // empty height like "100x" is accepted as height 0 — ported faithfully.
    if !h_rest.is_empty() {
        return false;
    }

    let w = atoi_u16(w_digits);
    let h = atoi_u16(h_digits);
    if w < h {
        return false;
    }

    out.screen_width = w;
    out.screen_height = h;
    true
}

/// Parses `<digits>dp` into the value; `None` if malformed.
fn parse_dp_value(s: &str) -> Option<u16> {
    let (digits, rest) = split_digits_prefix(s);
    if digits.is_empty() || rest != "dp" {
        return None;
    }
    Some(atoi_u16(digits))
}

fn parse_smallest_screen_width_dp(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.smallest_screen_width_dp = ConfigDescription::SCREENWIDTH_ANY;
        return true;
    }
    let rest = match name.strip_prefix("sw") {
        Some(r) => r,
        None => return false,
    };
    match parse_dp_value(rest) {
        Some(v) => {
            out.smallest_screen_width_dp = v;
            true
        }
        None => false,
    }
}

fn parse_screen_width_dp(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.screen_width_dp = ConfigDescription::SCREENWIDTH_ANY;
        return true;
    }
    let rest = match name.strip_prefix('w') {
        Some(r) => r,
        None => return false,
    };
    match parse_dp_value(rest) {
        Some(v) => {
            out.screen_width_dp = v;
            true
        }
        None => false,
    }
}

fn parse_screen_height_dp(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.screen_height_dp = ConfigDescription::SCREENWIDTH_ANY;
        return true;
    }
    let rest = match name.strip_prefix('h') {
        Some(r) => r,
        None => return false,
    };
    match parse_dp_value(rest) {
        Some(v) => {
            out.screen_height_dp = v;
            true
        }
        None => false,
    }
}

fn parse_version(name: &str, out: &mut ConfigDescription) -> bool {
    if name == WILDCARD_NAME {
        out.sdk_version = ConfigDescription::SDKVERSION_ANY;
        out.minor_version = ConfigDescription::MINORVERSION_ANY;
        return true;
    }
    let rest = match name.strip_prefix('v') {
        Some(r) => r,
        None => return false,
    };
    let (digits, tail) = split_digits_prefix(rest);
    if digits.is_empty() || !tail.is_empty() {
        return false;
    }
    out.sdk_version = atoi_u16(digits);
    out.minor_version = 0;
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ConfigDescription {
        ConfigDescription::parse(s).unwrap_or_else(|| panic!("invalid configuration: {s:?}"))
    }

    fn round_trip(s: &str) -> String {
        parse(s).to_string()
    }

    // --- ConfigDescription_test.cpp ports ------------------------------

    #[test]
    fn parse_fail_when_qualifiers_are_out_of_order() {
        assert_eq!(ConfigDescription::parse("en-sw600dp-ldrtl"), None);
        assert_eq!(ConfigDescription::parse("land-en"), None);
        assert_eq!(ConfigDescription::parse("hdpi-320dpi"), None);
    }

    #[test]
    fn parse_fail_when_qualifiers_are_not_matched() {
        assert_eq!(ConfigDescription::parse("en-sw600dp-ILLEGAL"), None);
    }

    #[test]
    fn parse_fail_when_qualifiers_have_trailing_dash() {
        assert_eq!(ConfigDescription::parse("en-sw600dp-land-"), None);
    }

    #[test]
    fn parse_basic_qualifiers() {
        assert_eq!(round_trip(""), "");
        assert_eq!(round_trip("fr-land"), "fr-land");
        assert_eq!(
            round_trip(
                "mcc310-pl-sw720dp-normal-long-port-night-xhdpi-keyssoft-qwerty-\
                 navexposed-nonav"
            ),
            "mcc310-pl-sw720dp-normal-long-port-night-xhdpi-keyssoft-qwerty-\
             navexposed-nonav-v13"
        );
    }

    #[test]
    fn parse_locales() {
        assert_eq!(round_trip("en-rUS"), "en-rUS");
    }

    #[test]
    fn parse_qualifier_added_in_api13() {
        assert_eq!(round_trip("sw600dp"), "sw600dp-v13");
        assert_eq!(round_trip("sw600dp-v8"), "sw600dp-v13");
    }

    #[test]
    fn parse_car_attribute() {
        let config = parse("car");
        assert_eq!(config.ui_mode, ConfigDescription::UI_MODE_TYPE_CAR);
    }

    #[test]
    fn parse_round_qualifier() {
        let config = parse("round");
        assert_eq!(
            config.screen_layout2 & ConfigDescription::MASK_SCREENROUND,
            ConfigDescription::SCREENROUND_YES
        );
        assert_eq!(config.sdk_version, SDK_MARSHMALLOW);
        assert_eq!(config.to_string(), "round-v23");

        let config = parse("notround");
        assert_eq!(
            config.screen_layout2 & ConfigDescription::MASK_SCREENROUND,
            ConfigDescription::SCREENROUND_NO
        );
        assert_eq!(config.sdk_version, SDK_MARSHMALLOW);
        assert_eq!(config.to_string(), "notround-v23");
    }

    #[test]
    fn parse_wide_color_gamut_qualifier() {
        let config = parse("widecg");
        assert_eq!(
            config.color_mode & ConfigDescription::MASK_WIDE_COLOR_GAMUT,
            ConfigDescription::WIDE_COLOR_GAMUT_YES
        );
        assert_eq!(config.sdk_version, SDK_O);
        assert_eq!(config.to_string(), "widecg-v26");

        let config = parse("nowidecg");
        assert_eq!(
            config.color_mode & ConfigDescription::MASK_WIDE_COLOR_GAMUT,
            ConfigDescription::WIDE_COLOR_GAMUT_NO
        );
        assert_eq!(config.sdk_version, SDK_O);
        assert_eq!(config.to_string(), "nowidecg-v26");
    }

    #[test]
    fn parse_hdr_qualifier() {
        let config = parse("highdr");
        assert_eq!(
            config.color_mode & ConfigDescription::MASK_HDR,
            ConfigDescription::HDR_YES
        );
        assert_eq!(config.sdk_version, SDK_O);
        assert_eq!(config.to_string(), "highdr-v26");

        let config = parse("lowdr");
        assert_eq!(
            config.color_mode & ConfigDescription::MASK_HDR,
            ConfigDescription::HDR_NO
        );
        assert_eq!(config.sdk_version, SDK_O);
        assert_eq!(config.to_string(), "lowdr-v26");
    }

    #[test]
    fn parse_vr_attribute() {
        let config = parse("vrheadset");
        assert_eq!(config.ui_mode, ConfigDescription::UI_MODE_TYPE_VR_HEADSET);
        assert_eq!(config.sdk_version, SDK_O);
        assert_eq!(config.to_string(), "vrheadset-v26");
    }

    #[test]
    fn parse_grammatical_gender_qualifier() {
        let config = parse("feminine");
        assert_eq!(
            config.grammatical_inflection,
            ConfigDescription::GRAMMATICAL_GENDER_FEMININE
        );
        assert_eq!(config.sdk_version, SDK_U);
        assert_eq!(config.to_string(), "feminine-v34");

        let config = parse("masculine");
        assert_eq!(
            config.grammatical_inflection,
            ConfigDescription::GRAMMATICAL_GENDER_MASCULINE
        );
        assert_eq!(config.sdk_version, SDK_U);
        assert_eq!(config.to_string(), "masculine-v34");

        let config = parse("neuter");
        assert_eq!(
            config.grammatical_inflection,
            ConfigDescription::GRAMMATICAL_GENDER_NEUTER
        );
        assert_eq!(config.sdk_version, SDK_U);
        assert_eq!(config.to_string(), "neuter-v34");
    }

    #[test]
    fn range_qualifiers_do_not_conflict() {
        assert!(!parse("large").conflicts_with(&parse("normal-land")));
        assert!(!parse("long-hdpi").conflicts_with(&parse("xhdpi")));
        assert!(!parse("sw600dp").conflicts_with(&parse("sw700dp")));
        assert!(!parse("v11").conflicts_with(&parse("v21")));
        assert!(!parse("h600dp").conflicts_with(&parse("h300dp")));
        assert!(!parse("w400dp").conflicts_with(&parse("w300dp")));
        assert!(!parse("600x400").conflicts_with(&parse("300x200")));
    }

    // --- version compatibility bumps ------------------------------------

    #[test]
    fn version_for_compatibility_table() {
        // orientation does not require a version.
        assert_eq!(round_trip("land"), "land");
        // screen size class / long / density => v4.
        assert_eq!(round_trip("hdpi"), "hdpi-v4");
        assert_eq!(round_trip("large"), "large-v4");
        assert_eq!(round_trip("notlong"), "notlong-v4");
        // ui mode => v8.
        assert_eq!(round_trip("night"), "night-v8");
        assert_eq!(round_trip("desk"), "desk-v8");
        // sw/w/h dp => v13.
        assert_eq!(round_trip("sw600dp"), "sw600dp-v13");
        assert_eq!(round_trip("w1024dp"), "w1024dp-v13");
        assert_eq!(round_trip("h720dp"), "h720dp-v13");
        // anydpi => v21.
        assert_eq!(round_trip("anydpi"), "anydpi-v21");
        // round => v23.
        assert_eq!(round_trip("round"), "round-v23");
        // vr / color mode => v26.
        assert_eq!(round_trip("vrheadset"), "vrheadset-v26");
        assert_eq!(round_trip("widecg"), "widecg-v26");
        assert_eq!(round_trip("highdr"), "highdr-v26");
        // grammatical gender => v34.
        assert_eq!(round_trip("feminine"), "feminine-v34");
        // An explicit higher version is preserved.
        assert_eq!(round_trip("sw600dp-v21"), "sw600dp-v21");
        // The full chain example from the task statement.
        assert_eq!(
            round_trip("sw600dp-land-night-hdpi-v21"),
            "sw600dp-land-night-hdpi-v21"
        );
    }

    // --- locale forms ----------------------------------------------------

    #[test]
    fn parse_modified_bcp47_qualifiers() {
        assert_eq!(round_trip("b+sr+Latn"), "b+sr+Latn");
        assert_eq!(round_trip("b+en+Latn+US"), "b+en+Latn+US");
        // Language+region only collapses back to the legacy form.
        assert_eq!(round_trip("b+en+US"), "en-rUS");
        // 3-letter language codes round-trip through the packed encoding.
        assert_eq!(round_trip("fil"), "fil");
        let config = parse("b+sr+Latn");
        assert_eq!(config.unpack_language(), "sr");
        assert_eq!(&config.locale_script, b"Latn");
        assert!(!config.locale_script_was_computed);
        // Malformed modified BCP-47 tags fail to parse.
        assert_eq!(ConfigDescription::parse("b+en+Latn+US+POSIX+x+x"), None);
    }

    #[test]
    fn bcp47_locale_strings() {
        assert_eq!(parse("en-rUS").get_bcp47_locale(false), "en-US");
        assert_eq!(parse("b+sr+Latn").get_bcp47_locale(false), "sr-Latn");
        assert_eq!(parse("b+en+Latn+US").get_bcp47_locale(false), "en-Latn-US");
        assert_eq!(parse("").get_bcp47_locale(false), "");
        // Tagalog canonicalizes to Filipino.
        assert_eq!(parse("tl").get_bcp47_locale(true), "fil");
        assert_eq!(parse("tl").get_bcp47_locale(false), "tl");
    }

    #[test]
    fn set_bcp47_locale_with_numbering_system() {
        let mut config = ConfigDescription::default();
        config.set_bcp47_locale("en-US-u-nu-arab");
        assert_eq!(config.unpack_language(), "en");
        assert_eq!(config.unpack_region(), "US");
        assert!(config.locale_script_was_computed);
        assert_eq!(&config.locale_numbering_system[..4], b"arab");
        assert_eq!(config.get_bcp47_locale(false), "en-US-u-nu-arab");
        // The dir-locale form switches to the modified BCP-47 syntax.
        assert_eq!(config.to_string(), "b+en+US+u+nu+arab");

        let mut config = ConfigDescription::default();
        config.set_bcp47_locale("sr-Latn-RS");
        assert_eq!(config.unpack_language(), "sr");
        assert_eq!(config.unpack_region(), "RS");
        assert_eq!(&config.locale_script, b"Latn");
        assert!(!config.locale_script_was_computed);
        assert_eq!(config.to_string(), "b+sr+Latn+RS");
    }

    #[test]
    fn three_letter_pack_round_trip() {
        let mut config = ConfigDescription::default();
        config.pack_language(b"fil");
        assert_eq!(config.language, [0xAD, 0x05]); // kFilipino
        assert_eq!(config.unpack_language(), "fil");

        let mut config = ConfigDescription::default();
        config.pack_region(b"419");
        assert_eq!(config.country, [0xA4, 0x24]);
        assert_eq!(config.unpack_region(), "419");
    }

    // --- mcc / mnc ---------------------------------------------------------

    #[test]
    fn mcc_mnc_parsing() {
        assert_eq!(round_trip("mcc310"), "mcc310");
        // Leading zeros in mnc collapse (atoi), and mnc00 maps to MNC_ZERO.
        assert_eq!(round_trip("mcc310-mnc004"), "mcc310-mnc4");
        let config = parse("mcc310-mnc00");
        assert_eq!(config.mnc, MNC_ZERO);
        assert_eq!(config.to_string(), "mcc310-mnc65535");
        // mcc requires exactly three digits, none of "31"/"3100" work.
        assert_eq!(ConfigDescription::parse("mcc31"), None);
        assert_eq!(ConfigDescription::parse("mcc3100"), None);
        assert_eq!(ConfigDescription::parse("mcc000"), None);
        // mnc takes 1..=3 digits.
        assert_eq!(ConfigDescription::parse("mnc1234"), None);
    }

    // --- screen size ---------------------------------------------------------

    #[test]
    fn screen_size_parsing() {
        assert_eq!(round_trip("1024x768"), "1024x768");
        let config = parse("640x480");
        assert_eq!(config.screen_width, 640);
        assert_eq!(config.screen_height, 480);
        // Width must be >= height.
        assert_eq!(ConfigDescription::parse("480x640"), None);
    }

    // --- binary form -----------------------------------------------------

    #[test]
    fn bytes_layout_and_offsets() {
        let mut c = ConfigDescription::default();
        c.mcc = 0x0102;
        c.mnc = 0x0304;
        c.language = [b'e', b'n'];
        c.country = [b'U', b'S'];
        c.orientation = 1;
        c.touchscreen = 2;
        c.density = 0x1234;
        c.keyboard = 3;
        c.navigation = 4;
        c.input_flags = 5;
        c.grammatical_inflection = 2;
        c.screen_width = 0x0506;
        c.screen_height = 0x0708;
        c.sdk_version = 21;
        c.minor_version = 1;
        c.screen_layout = 0x40;
        c.ui_mode = 0x21;
        c.smallest_screen_width_dp = 600;
        c.screen_width_dp = 600;
        c.screen_height_dp = 1024;
        c.locale_script = *b"Latn";
        c.locale_variant = *b"variant\0";
        c.screen_layout2 = 2;
        c.color_mode = 5;
        c.screen_config_pad2 = 0xBEEF;
        c.locale_script_was_computed = true;
        c.locale_numbering_system = *b"arab\0\0\0\0";

        let b = c.to_bytes();
        assert_eq!(b.len(), ConfigDescription::SIZE);
        assert_eq!(&b[0..4], &(ConfigDescription::SIZE as u32).to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 0x0102); // mcc
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 0x0304); // mnc
        assert_eq!(&b[8..12], b"enUS"); // language + country
        assert_eq!(b[12], 1); // orientation
        assert_eq!(b[13], 2); // touchscreen
        assert_eq!(u16::from_le_bytes([b[14], b[15]]), 0x1234); // density
        assert_eq!(b[16], 3); // keyboard
        assert_eq!(b[17], 4); // navigation
        assert_eq!(b[18], 5); // inputFlags
        assert_eq!(b[19], 2); // grammaticalInflection
        assert_eq!(u16::from_le_bytes([b[20], b[21]]), 0x0506); // screenWidth
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), 21); // sdkVersion
        assert_eq!(b[28], 0x40); // screenLayout
        assert_eq!(b[29], 0x21); // uiMode
        assert_eq!(u16::from_le_bytes([b[30], b[31]]), 600); // smallestScreenWidthDp
        assert_eq!(u16::from_le_bytes([b[32], b[33]]), 600); // screenWidthDp
        assert_eq!(u16::from_le_bytes([b[34], b[35]]), 1024); // screenHeightDp
        assert_eq!(&b[36..40], b"Latn"); // localeScript
        assert_eq!(&b[40..48], b"variant\0"); // localeVariant
        assert_eq!(b[48], 2); // screenLayout2
        assert_eq!(b[49], 5); // colorMode
        assert_eq!(u16::from_le_bytes([b[50], b[51]]), 0xBEEF); // screenConfigPad2
        assert_eq!(b[52], 1); // localeScriptWasComputed
        assert_eq!(&b[53..61], b"arab\0\0\0\0"); // localeNumberingSystem
        assert_eq!(&b[61..64], &[0, 0, 0]); // padding

        assert_eq!(ConfigDescription::from_bytes(&b), Some(c));
    }

    #[test]
    fn bytes_round_trip_for_parsed_configs() {
        for s in [
            "",
            "en",
            "en-rUS",
            "b+sr+Latn",
            "b+en+Latn+US",
            "fil",
            "mcc310-mnc004",
            "sw600dp-land-night-hdpi-v21",
            "mcc310-pl-sw720dp-normal-long-port-night-xhdpi-keyssoft-qwerty-navexposed-nonav",
            "feminine",
            "ldrtl",
            "watch",
            "1024x768",
            "round-notnight-560dpi",
            "anydpi",
            "nodpi",
        ] {
            let config = parse(s);
            let bytes = config.to_bytes();
            assert_eq!(bytes.len(), ConfigDescription::SIZE, "size for {s:?}");
            let back = ConfigDescription::from_bytes(&bytes)
                .unwrap_or_else(|| panic!("from_bytes failed for {s:?}"));
            assert_eq!(back, config, "byte round-trip for {s:?}");
        }
    }

    #[test]
    fn from_bytes_tolerates_smaller_and_larger_sizes() {
        let config = parse("en-rUS-sw600dp");
        let full = config.to_bytes();

        // Older, smaller struct (e.g. 52 bytes: pre-numbering-system): the
        // dropped fields are zero here, so the config must decode equal.
        let mut small = full[..52].to_vec();
        small[0..4].copy_from_slice(&52u32.to_le_bytes());
        assert_eq!(ConfigDescription::from_bytes(&small), Some(config));

        // 36-byte (pre-localeScript) variant.
        let mut tiny = full[..36].to_vec();
        tiny[0..4].copy_from_slice(&36u32.to_le_bytes());
        assert_eq!(ConfigDescription::from_bytes(&tiny), Some(config));

        // Larger, newer struct: extra trailing bytes are ignored.
        let mut big = full.clone();
        big.extend_from_slice(&[0xAA; 8]);
        big[0..4].copy_from_slice(&72u32.to_le_bytes());
        assert_eq!(ConfigDescription::from_bytes(&big), Some(config));

        // Declared size larger than the available data: clamped safely.
        let mut short = full.clone();
        short.truncate(40);
        assert!(ConfigDescription::from_bytes(&short).is_some());

        // Not even a size field.
        assert_eq!(ConfigDescription::from_bytes(&[1, 2]), None);
    }

    // --- compare / ordering ------------------------------------------------

    #[test]
    fn compare_ordering() {
        let mut configs = [
            parse("mcc310"),
            parse("en-rUS"),
            parse("v21"),
            parse(""),
            parse("en"),
        ];
        configs.sort();
        let strings: Vec<String> = configs.iter().map(|c| c.to_string()).collect();
        assert_eq!(strings, vec!["", "v21", "en", "en-rUS", "mcc310"]);

        assert!(parse("v13") < parse("v21"));
        assert!(parse("") < parse("en"));
        assert_eq!(parse("en-rUS").compare(&parse("en-rUS")), Ordering::Equal);
    }

    #[test]
    fn compare_logical_ordering() {
        // mcc sorts first in logical order.
        assert_eq!(
            parse("mcc310").compare_logical(&parse("en")),
            Ordering::Greater
        );
        assert_eq!(parse("en").compare_logical(&parse("fr")), Ordering::Less);
        assert_eq!(
            parse("sw600dp").compare_logical(&parse("sw720dp")),
            Ordering::Less
        );
        assert_eq!(parse("v13").compare_logical(&parse("v21")), Ordering::Less);
    }

    #[test]
    fn diff_bits() {
        assert_eq!(
            parse("en").diff(&parse("fr")),
            ConfigDescription::CONFIG_LOCALE
        );
        assert_eq!(
            parse("v4").diff(&parse("")),
            ConfigDescription::CONFIG_VERSION
        );
        // hdpi/mdpi differ only in density (both get v4).
        assert_eq!(
            parse("hdpi").diff(&parse("mdpi")),
            ConfigDescription::CONFIG_DENSITY
        );
        assert_eq!(
            parse("hdpi").diff(&parse("")),
            ConfigDescription::CONFIG_DENSITY | ConfigDescription::CONFIG_VERSION
        );
        assert_eq!(
            parse("land").diff(&parse("port")),
            ConfigDescription::CONFIG_ORIENTATION
        );
        assert_eq!(
            parse("sw600dp-v13").diff(&parse("sw720dp-v13")),
            ConfigDescription::CONFIG_SMALLEST_SCREEN_SIZE
        );
        assert_eq!(
            parse("ldrtl").diff(&parse("ldltr")),
            ConfigDescription::CONFIG_LAYOUTDIR
        );
        assert_eq!(parse("en").diff(&parse("en")), 0);
    }

    // --- specificity / precedence ------------------------------------------

    #[test]
    fn more_specific() {
        let default = parse("");
        let en = parse("en");
        let en_us = parse("en-rUS");
        assert!(en.is_more_specific_than(&default));
        assert!(en_us.is_more_specific_than(&en));
        assert!(!default.is_more_specific_than(&en));
        assert!(!en.is_more_specific_than(&en_us));
        assert!(parse("sw600dp").is_more_specific_than(&default));
        // Equal configs are not more specific than each other.
        assert!(!en.is_more_specific_than(&parse("en")));
    }

    #[test]
    fn higher_precedence() {
        // "en" has higher precedence than "v23".
        assert!(parse("en").has_higher_precedence_than(&parse("v23")));
        assert!(!parse("v23").has_higher_precedence_than(&parse("en")));
        // "en" and "en-v23" have the same precedence.
        assert!(!parse("en").has_higher_precedence_than(&parse("en-v23")));
        assert!(!parse("en-v23").has_higher_precedence_than(&parse("en")));
        // mcc trumps everything.
        assert!(parse("mcc310").has_higher_precedence_than(&parse("en")));
    }

    // --- match / better-than -------------------------------------------------

    #[test]
    fn match_basics() {
        let request = parse("en-rUS-sw600dp-land-night-xhdpi-v21");
        assert!(parse("").matches(&request));
        assert!(parse("en").matches(&request));
        assert!(parse("en-rUS").matches(&request));
        assert!(parse("land").matches(&request));
        assert!(parse("sw600dp").matches(&request));
        assert!(parse("night").matches(&request));
        assert!(parse("v13").matches(&request));

        assert!(!parse("fr").matches(&request));
        assert!(!parse("en-rGB").matches(&request));
        assert!(!parse("port").matches(&request));
        assert!(!parse("sw720dp").matches(&request));
        assert!(!parse("notnight").matches(&request));
        assert!(!parse("v23").matches(&request));

        // Tagalog and Filipino are equivalent languages.
        assert!(parse("tl").matches(&parse("fil")));

        // A default piece of data matches every request, but a specific
        // config does not match a default request.
        assert!(!parse("en").matches(&parse("")));
    }

    #[test]
    fn match_keys_hidden_compat() {
        // A request for KEYSHIDDEN_NO also matches KEYSHIDDEN_SOFT.
        let soft_request = parse("keyssoft");
        assert!(parse("keysexposed").matches(&soft_request));
        assert!(!parse("keyshidden").matches(&soft_request));
    }

    #[test]
    fn better_than_density() {
        let req_hdpi = parse("hdpi");
        assert!(parse("hdpi").is_better_than(&parse("mdpi"), Some(&req_hdpi)));
        assert!(!parse("mdpi").is_better_than(&parse("hdpi"), Some(&req_hdpi)));
        // DENSITY_ANY is always preferred over scaling a bucket.
        let req_xhdpi = parse("xhdpi");
        assert!(parse("anydpi-v21").is_better_than(&parse("hdpi"), Some(&req_xhdpi)));
        assert!(!parse("hdpi").is_better_than(&parse("anydpi-v21"), Some(&req_xhdpi)));
        // Prefer scaling down: between xhdpi and ldpi for an hdpi request,
        // xhdpi wins.
        assert!(parse("xhdpi").is_better_than(&parse("ldpi"), Some(&req_hdpi)));
    }

    #[test]
    fn better_than_general() {
        let request = parse("en-rUS-land-v21");
        assert!(parse("land").is_better_than(&parse(""), Some(&request)));
        assert!(!parse("").is_better_than(&parse("land"), Some(&request)));
        assert!(parse("en").is_better_than(&parse(""), Some(&request)));
        assert!(parse("v21").is_better_than(&parse("v13"), Some(&request)));
        // Without a request this degenerates to is_more_specific_than.
        assert!(parse("en-rUS").is_better_than(&parse("en"), None));
        assert!(!parse("en").is_better_than(&parse("en-rUS"), None));
    }

    #[test]
    fn locale_better_than_us_english_special_case() {
        // For a US English request, a no-locale resource beats a resource
        // with a non-US country.
        let request = parse("en-rUS");
        let no_locale = parse("");
        let en_gb = parse("en-rGB");
        let en_us = parse("en-rUS");
        assert!(no_locale.is_locale_better_than(&en_gb, &request));
        assert!(!en_gb.is_locale_better_than(&no_locale, &request));
        assert!(en_us.is_locale_better_than(&no_locale, &request));
    }

    // --- dominates / conflicts / compatible ----------------------------------

    #[test]
    fn dominates() {
        // The default config dominates any config it can match (no locale
        // or mcc/mnc difference).
        assert!(parse("").dominates(&parse("hdpi")));
        assert!(parse("").dominates(&parse("land")));
        // Locale/mcc/mnc differences disable domination (b/62409213,
        // b/171892595).
        assert!(!parse("").dominates(&parse("en")));
        assert!(!parse("").dominates(&parse("mcc310")));
        // Identical configs dominate each other.
        assert!(parse("land").dominates(&parse("land")));
        // A smaller sw-dp range dominates a larger one.
        assert!(parse("sw600dp").dominates(&parse("sw700dp")));
        assert!(!parse("sw700dp").dominates(&parse("sw600dp")));
        // Unrelated qualifiers do not dominate.
        assert!(!parse("land").dominates(&parse("hdpi")));
    }

    #[test]
    fn conflicts() {
        assert!(parse("land").conflicts_with(&parse("port")));
        assert!(parse("night").conflicts_with(&parse("notnight")));
        assert!(parse("ldltr").conflicts_with(&parse("ldrtl")));
        assert!(!parse("land").conflicts_with(&parse("land")));
        assert!(!parse("land").conflicts_with(&parse("night")));
    }

    #[test]
    fn compatible() {
        // From the C++ header docs: land-v11 conflicts with port-v21 but is
        // compatible with v21 (both land-v11 and v21 match en-land-v23).
        assert!(!parse("land-v11").is_compatible_with(&parse("port-v21")));
        assert!(parse("land-v11").is_compatible_with(&parse("v21")));
        assert!(!parse("land").is_compatible_with(&parse("port")));
    }

    // --- misc ------------------------------------------------------------

    #[test]
    fn default_and_copy_without_sdk() {
        assert!(parse("").is_default());
        assert!(!parse("v21").is_default());
        assert!(!parse("en").is_default());
        let config = parse("sw600dp");
        assert_eq!(config.sdk_version, SDK_HONEYCOMB_MR2);
        let stripped = config.copy_without_sdk_version();
        assert_eq!(stripped.sdk_version, 0);
        assert_eq!(stripped.smallest_screen_width_dp, 600);
    }

    #[test]
    fn match_with_density() {
        // match_with_density additionally requires
        // (self.density == 0 || o.density != 0).
        assert!(parse("").match_with_density(&parse("hdpi")));
        // Both densities set: plain match() rules apply (density always
        // matches).
        assert!(parse("hdpi").match_with_density(&parse("xhdpi-v4")));
        // self has a density but o does not: refused even though match()
        // would succeed.
        assert!(parse("hdpi").matches(&parse("v4")));
        assert!(!parse("hdpi").match_with_density(&parse("v4")));
        // density==0 in self matches regardless of o's density.
        assert!(parse("land").match_with_density(&parse("land-hdpi")));
    }

    #[test]
    fn wildcard_any_parts() {
        // "any" is consumed by the first parser in sequence (mcc), then mnc,
        // etc. A string of "any" parts parses to the default config.
        let config = parse("any-any");
        assert!(config.is_default());
        assert_eq!(round_trip("any"), "");
    }

    #[test]
    fn display_uses_dir_locale_forms() {
        // Legacy locale forms with surrounding qualifiers.
        assert_eq!(round_trip("mcc310-en-rUS-land"), "mcc310-en-rUS-land");
        // Modified BCP-47 keeps its position between mnc and layout dir.
        assert_eq!(
            round_trip("mcc310-mnc004-b+sr+Latn-ldrtl"),
            "mcc310-mnc4-b+sr+Latn-ldrtl"
        );
    }
}
