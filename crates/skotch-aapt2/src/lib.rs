//! skotch-aapt2 — a native reimplementation of the Android Asset
//! Packaging Tool (aapt2).
//!
//! Ported from the AOSP sources at `frameworks/base/tools/aapt2`,
//! covering the modern tool surface: `compile`, `link`, `dump`,
//! `optimize`, `convert`, `diff`, and `version` (legacy/deprecated
//! options are intentionally dropped). Everything is exposed as a
//! library API so the skotch build pipeline can run resource
//! processing in-process; the `skotch aapt2` subcommand is a thin CLI
//! over these functions with aapt2-compatible flags.
//!
//! Module map (mirroring the C++ tree):
//!
//! - [`res`] — the resource model: names/IDs ([`res`]),
//!   configurations ([`res::config`]), values ([`res::value`]), the
//!   table ([`res::table`]), string pools ([`res::string_pool`]).
//! - [`pb`] — protobuf wire codec + `Resources.proto` conversions
//!   (`format/proto/`).
//! - [`container`] — the AAPT2 container (`.flat`/`.apc`) format
//!   (`format/Container.cpp`).
//! - [`xml`] — XML DOM, source parsing, binary XML (AXML) flatten +
//!   parse (`xml/`, `format/binary/XmlFlattener.cpp`).
//! - [`util`] — string processing shared across phases
//!   (`ResourceUtils::StringBuilder` etc.).

pub mod apk;
pub mod binary;
pub mod cli;
pub mod compile;
pub mod container;
pub mod convert;
pub mod diag;
pub mod dump;
pub mod link;
pub mod optimize;
pub mod pb;
pub mod res;
pub mod util;
pub mod xml;
