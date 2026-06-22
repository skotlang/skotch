//! Pseudolocalization (`--pseudo-localize`): generates en-XA
//! (accented/expanded) and ar-XB (RTL-wrapped) variants of default
//! strings and plurals.
//!
//! Port of `compile/PseudolocaleGenerator.cpp` + `Pseudolocalizer.cpp`.
//!
//! Status: not yet implemented — `--pseudo-localize` reports a clear
//! error instead of silently dropping the locales. Tracked as a
//! follow-up port; the call site in [`super::compile_table`] is wired.

use crate::res::table::ResourceTable;
use anyhow::{bail, Result};

pub fn generate_pseudolocales(
    _table: &mut ResourceTable,
    _grammatical_gender_values: &str,
    _grammatical_gender_ratio: &str,
) -> Result<()> {
    bail!("--pseudo-localize is not yet supported by skotch aapt2");
}
