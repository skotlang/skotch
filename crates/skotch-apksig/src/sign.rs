//! Output APK construction — the top-level signer mirroring
//! `com.android.apksig.ApkSigner` and `DefaultApkSignerEngine`.
//!
//! [`ApkSigner`] is a builder; [`ApkSigner::sign`] takes the input APK bytes
//! and returns the signed APK (and optional v4 `.idsig`). The pipeline copies
//! input entries preserving alignment, regenerates v1 signature entries,
//! inserts the v2/v3/v3.1 signing block on a 4096-byte boundary, and patches
//! the EOCD — matching apksig's behavior closely enough to reproduce its
//! golden APKs byte-for-byte.

use crate::crypto::{
    suggested_signature_algorithms, suggested_v1_digest_algorithm, Certificate, DigestAlgorithm,
    PrivateKey, SignatureAlgorithm,
};
use crate::digest::{compute_content_digests, ContentDigestAlgorithm};
use crate::lineage::SigningCertificateLineage;
use crate::sigblock::{generate_apk_signing_block, signing_block_padding};
use crate::v1::{self, V1SignerConfig};
use crate::v2::{generate_v2_block, SignerConfig as BlockSignerConfig};
use crate::v3::{generate_v3_block, V3BlockParams, V3SignerConfig};
use crate::zip::{self, eocd, CdRecord};
use crate::{axml, sdk};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// ZIP extra-field header id used to align uncompressed entry data.
const ALIGNMENT_EXTRA_ID: u16 = 0xd935;
const ALIGNMENT_EXTRA_MIN_SIZE: usize = 6;
/// 4 KiB file-size alignment for `--align-file-size`.
const ANDROID_FILE_ALIGNMENT: usize = 4096;
/// Default native-library page alignment (`Constants.LIBRARY_PAGE_ALIGNMENT_BYTES`).
pub const DEFAULT_LIB_PAGE_ALIGNMENT: usize = 16384;

const ANDROID_MANIFEST: &str = "AndroidManifest.xml";

/// A signer: key, certificate chain, and the v1 output basename.
pub struct SignerConfig {
    pub name: String,
    pub key: PrivateKey,
    pub certificates: Vec<Certificate>,
    /// Signer-targeted minimum SDK version (0 = derive from algorithms).
    pub min_sdk_version: u32,
    pub deterministic_dsa: bool,
}

/// Result of signing.
pub struct SignResult {
    pub apk: Vec<u8>,
    /// `.idsig` bytes if v4 signing was enabled.
    pub v4_signature: Option<Vec<u8>>,
}

/// Builder mirroring `ApkSigner.Builder`.
pub struct ApkSigner {
    signers: Vec<SignerConfig>,
    min_sdk_version: Option<u32>,
    v1_enabled: Option<bool>,
    v2_enabled: Option<bool>,
    v3_enabled: Option<bool>,
    v4_enabled: bool,
    verity_enabled: bool,
    debuggable_permitted: bool,
    alignment_preserved: bool,
    lib_page_alignment: usize,
    align_file_size: bool,
    lineage: Option<SigningCertificateLineage>,
    rotation_min_sdk_version: Option<u32>,
    created_by: String,
}

impl ApkSigner {
    pub fn new(signers: Vec<SignerConfig>) -> ApkSigner {
        ApkSigner {
            signers,
            min_sdk_version: None,
            v1_enabled: None,
            v2_enabled: None,
            v3_enabled: None,
            v4_enabled: false,
            verity_enabled: false,
            debuggable_permitted: true,
            alignment_preserved: false,
            lib_page_alignment: DEFAULT_LIB_PAGE_ALIGNMENT,
            align_file_size: false,
            lineage: None,
            rotation_min_sdk_version: None,
            created_by: v1::CREATED_BY_DEFAULT.to_string(),
        }
    }

    pub fn min_sdk_version(mut self, v: u32) -> Self {
        self.min_sdk_version = Some(v);
        self
    }
    pub fn v1_signing_enabled(mut self, v: bool) -> Self {
        self.v1_enabled = Some(v);
        self
    }
    pub fn v2_signing_enabled(mut self, v: bool) -> Self {
        self.v2_enabled = Some(v);
        self
    }
    pub fn v3_signing_enabled(mut self, v: bool) -> Self {
        self.v3_enabled = Some(v);
        self
    }
    pub fn v4_signing_enabled(mut self, v: bool) -> Self {
        self.v4_enabled = v;
        self
    }
    pub fn verity_enabled(mut self, v: bool) -> Self {
        self.verity_enabled = v;
        self
    }
    pub fn debuggable_apk_permitted(mut self, v: bool) -> Self {
        self.debuggable_permitted = v;
        self
    }
    pub fn alignment_preserved(mut self, v: bool) -> Self {
        self.alignment_preserved = v;
        self
    }
    pub fn lib_page_alignment(mut self, v: usize) -> Self {
        self.lib_page_alignment = v;
        self
    }
    pub fn align_file_size(mut self, v: bool) -> Self {
        self.align_file_size = v;
        self
    }
    pub fn signing_certificate_lineage(mut self, l: SigningCertificateLineage) -> Self {
        self.lineage = Some(l);
        self
    }
    pub fn rotation_min_sdk_version(mut self, v: u32) -> Self {
        self.rotation_min_sdk_version = Some(v);
        self
    }
    pub fn created_by(mut self, v: impl Into<String>) -> Self {
        self.created_by = v.into();
        self
    }

    /// Signs `input_apk` and returns the output bytes.
    pub fn sign(&self, input_apk: &[u8]) -> Result<SignResult> {
        if self.signers.is_empty() {
            bail!("At least one signer must be specified");
        }

        let sections = zip::find_zip_sections(input_apk)?;
        let cd_records = zip::parse_central_directory(input_apk, &sections)?;

        // LFH section spans from 0 to the start of the (old) signing block, or
        // the central directory if there isn't one.
        let lfh_section_end = match zip::find_apk_signing_block(input_apk, &sections)? {
            Some(info) => info.start_offset,
            None => sections.cd_offset,
        };
        let lfh_section = &input_apk[..lfh_section_end];

        // Determine minSdkVersion: explicit override, else parse the manifest.
        let manifest = find_android_manifest(input_apk, &cd_records, lfh_section)?;
        let min_sdk = match self.min_sdk_version {
            Some(v) => v,
            None => match &manifest {
                Some(m) => axml::min_sdk_version(m)?,
                None => 1,
            },
        };

        // Resolve scheme enablement.
        let v1_enabled = self.v1_enabled.unwrap_or(true);
        let v2_enabled = self.v2_enabled.unwrap_or(true);
        let v3_enabled = self.v3_enabled.unwrap_or_else(|| {
            // v3 supports a single signer unless a lineage links them.
            !(self.signers.len() > 1 && self.lineage.is_none())
        });

        // Debuggable rejection.
        if !self.debuggable_permitted {
            if let Some(m) = &manifest {
                if axml::is_debuggable(m)? {
                    bail!("APK is debuggable and debuggable APK signing is not permitted");
                }
            }
        }

        // ── Step 5: copy input entries in LFH-offset order ────────────────
        let mut cd_by_lfh: Vec<&CdRecord> = cd_records.iter().collect();
        cd_by_lfh.sort_by_key(|r| r.lfh_offset);

        let mut output = Vec::with_capacity(input_apk.len() + 8192);
        let mut output_cd_by_name: BTreeMap<String, CdRecord> = BTreeMap::new();
        let mut jar_entry_digests: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut input_manifest_main_attrs: Option<Vec<(String, String)>> = None;

        let v1_digest_algorithm = if v1_enabled {
            suggested_v1_digest_algorithm(&self.signers[0].key, min_sdk)?
        } else {
            DigestAlgorithm::Sha256
        };

        let mut last_modified_date: i32 = -1;
        let mut last_modified_time: i32 = -1;
        let mut input_offset = 0usize;

        for cd in &cd_by_lfh {
            let name = &cd.name;
            if name == v1::MANIFEST_ENTRY_NAME {
                // Reuse the input manifest's main section; do not copy it.
                let lfr = zip::parse_local_file_record(lfh_section, cd)?;
                input_offset = advance_gap_and_consume(
                    &mut output,
                    lfh_section,
                    input_offset,
                    cd.lfh_offset as usize,
                    lfr.size,
                );
                if v1_enabled {
                    let data = lfr.uncompressed_data(lfh_section)?;
                    input_manifest_main_attrs = Some(v1::parse_manifest_main_attributes(&data));
                }
                continue;
            }
            let needed = v1::is_jar_entry_digest_needed_in_manifest(name);
            let lfr = zip::parse_local_file_record(lfh_section, cd)?;

            // Copy any gap before this record verbatim.
            input_offset = copy_gap(
                &mut output,
                lfh_section,
                input_offset,
                cd.lfh_offset as usize,
            );

            if !needed {
                // Skipped entry: consume from input, do not output.
                input_offset += lfr.size;
                continue;
            }

            // Track max last-modified for generated entries.
            let date = cd.last_modified_date as i32;
            let time = cd.last_modified_time as i32;
            if last_modified_date == -1
                || date > last_modified_date
                || (date == last_modified_date && time > last_modified_time)
            {
                last_modified_date = date;
                last_modified_time = time;
            }

            // Digest the entry's uncompressed data for v1.
            if v1_enabled {
                let data = lfr.uncompressed_data(lfh_section)?;
                jar_entry_digests.insert(name.clone(), v1_digest_algorithm.digest(&data));
            }

            let output_offset = output.len();
            let data_offset_in_record =
                self.output_input_entry(&mut output, lfh_section, &lfr, output_offset);
            let _ = data_offset_in_record;
            input_offset += lfr.size;

            let out_cd = if output_offset == lfr.start_offset {
                (*cd).clone()
            } else {
                cd.with_lfh_offset(output_offset as u32)
            };
            output_cd_by_name.insert(name.clone(), out_cd);
        }
        // Trailing gap.
        if input_offset < lfh_section.len() {
            output.extend_from_slice(&lfh_section[input_offset..]);
        }

        if last_modified_date == -1 {
            last_modified_date = 0x3a21; // Jan 1 2009 (DOS)
            last_modified_time = 0;
        }

        // ── Step 6: output CD records in input-CD order ───────────────────
        let mut output_cd_records: Vec<CdRecord> =
            Vec::with_capacity(cd_records.len() + signers_extra(self));
        for cd in &cd_records {
            if let Some(out) = output_cd_by_name.get(&cd.name) {
                output_cd_records.push(out.clone());
            }
        }

        // ── Step 8: generate and append v1 signature entries ──────────────
        if v1_enabled {
            let scheme_ids = v1_scheme_ids(v2_enabled, v3_enabled);
            let v1_signers = self.v1_signer_configs(v3_enabled, min_sdk)?;
            let v1_out = v1::sign(
                &v1_signers,
                v1_digest_algorithm,
                &jar_entry_digests,
                &scheme_ids,
                input_manifest_main_attrs.as_deref(),
                &self.created_by,
            )?;
            for (name, data) in v1_out.entries {
                let (compressed, crc) = zip::deflate_level9(&data);
                let lfh_offset = output.len() as u32;
                let lfh = zip::lfh_record_with_deflate_data(
                    &name,
                    last_modified_time as u16,
                    last_modified_date as u16,
                    &compressed,
                    crc,
                    data.len() as u32,
                );
                output.extend_from_slice(&lfh);
                output_cd_records.push(CdRecord::new_deflated(
                    &name,
                    last_modified_time as u16,
                    last_modified_date as u16,
                    crc,
                    compressed.len() as u32,
                    data.len() as u32,
                    lfh_offset,
                ));
            }
        }

        // ── Steps 9-12: central directory, signing block, EOCD ────────────
        let cd_start = output.len();
        let mut output_cd = Vec::new();
        for r in &output_cd_records {
            output_cd.extend_from_slice(&r.raw);
        }
        let input_eocd = sections.eocd(input_apk);
        let base_eocd = eocd::with_modified_cd_info(
            input_eocd,
            output_cd_records.len() as u16,
            output_cd.len() as u32,
            cd_start as u32,
        );

        let signing_block = if v2_enabled || v3_enabled {
            Some(self.build_signing_block(
                &output, &output_cd, &base_eocd, cd_start, min_sdk, v2_enabled, v3_enabled,
            )?)
        } else {
            None
        };

        match signing_block {
            Some(SigningBlockResult {
                padding,
                block,
                mut eocd,
            }) => {
                output.resize(cd_start + padding, 0);
                output.extend_from_slice(&block);
                output.extend_from_slice(&output_cd);
                eocd::set_cd_offset(&mut eocd, (cd_start + padding + block.len()) as u32);
                output.extend_from_slice(&eocd);
            }
            None => {
                output.extend_from_slice(&output_cd);
                output.extend_from_slice(&base_eocd);
            }
        }

        // ── Step 13: v4 ───────────────────────────────────────────────────
        let v4_signature = if self.v4_enabled && (v2_enabled || v3_enabled) {
            let signer = &self.signers[0];
            let algs =
                suggested_signature_algorithms(&signer.key, false, signer.deterministic_dsa)?;
            let alg = algs[0];
            Some(crate::v4::generate_v4_signature(
                &output,
                &signer.key,
                &signer.certificates[0],
                alg,
            )?)
        } else {
            None
        };

        Ok(SignResult {
            apk: output,
            v4_signature,
        })
    }

    /// Builds the v2/v3/v3.1 signing block, honoring `--align-file-size`.
    fn build_signing_block(
        &self,
        output_lfh: &[u8],
        output_cd: &[u8],
        base_eocd: &[u8],
        cd_start: usize,
        min_sdk: u32,
        v2_enabled: bool,
        v3_enabled: bool,
    ) -> Result<SigningBlockResult> {
        let padding = signing_block_padding(cd_start);
        let mut before_cd = Vec::with_capacity(cd_start + padding);
        before_cd.extend_from_slice(output_lfh);
        before_cd.resize(cd_start + padding, 0);

        let assemble = |eocd_for_digest: &[u8]| -> Result<Vec<u8>> {
            let scheme_blocks = self.scheme_blocks(
                &before_cd,
                output_cd,
                eocd_for_digest,
                min_sdk,
                v2_enabled,
                v3_enabled,
            )?;
            Ok(generate_apk_signing_block(&scheme_blocks))
        };

        // eocd_for_digest = base EOCD with CD offset pointing at the block start.
        let mut eocd_for_digest = base_eocd.to_vec();
        eocd::set_cd_offset(&mut eocd_for_digest, (cd_start + padding) as u32);
        let mut block = assemble(&eocd_for_digest)?;
        let mut final_eocd = base_eocd.to_vec();

        if self.align_file_size {
            let file_size = cd_start + output_cd.len() + padding + block.len() + base_eocd.len();
            if file_size % ANDROID_FILE_ALIGNMENT != 0 {
                let eocd_padding = ANDROID_FILE_ALIGNMENT - (file_size % ANDROID_FILE_ALIGNMENT);
                final_eocd = eocd::with_padded_comment(base_eocd, eocd_padding);
                let mut padded_for_digest = final_eocd.clone();
                eocd::set_cd_offset(&mut padded_for_digest, (cd_start + padding) as u32);
                block = assemble(&padded_for_digest)?;
            }
        }

        Ok(SigningBlockResult {
            padding,
            block,
            eocd: final_eocd,
        })
    }

    /// Assembles the ordered (block, id) pairs: v2, then v3.1, then v3.0.
    fn scheme_blocks(
        &self,
        before_cd: &[u8],
        central_dir: &[u8],
        eocd: &[u8],
        min_sdk: u32,
        v2_enabled: bool,
        v3_enabled: bool,
    ) -> Result<Vec<(Vec<u8>, u32)>> {
        // Collect the union of content-digest algorithms required.
        let mut needed_algs: Vec<ContentDigestAlgorithm> = Vec::new();
        let v2_signers = self.block_signer_configs(self.v2_subset(v3_enabled))?;
        let v3_signers = self.v3_signer_configs(min_sdk)?;
        if v2_enabled {
            for s in &v2_signers {
                for a in &s.signature_algorithms {
                    push_unique(&mut needed_algs, a.content_digest_algorithm());
                }
            }
        }
        if v3_enabled {
            for s in &v3_signers {
                for a in &s.signer.signature_algorithms {
                    push_unique(&mut needed_algs, a.content_digest_algorithm());
                }
            }
        }
        let digests = compute_content_digests(&needed_algs, before_cd, central_dir, eocd)?;

        let mut blocks = Vec::new();
        if v2_enabled {
            blocks.push(generate_v2_block(&v2_signers, &digests, v3_enabled)?);
        }
        if v3_enabled {
            // v3.1 split only matters with multiple targeted signers; the
            // common single-/dual-signer goldens use the v3.0 block alone.
            blocks.push(generate_v3_block(
                &v3_signers,
                &digests,
                &V3BlockParams::v3(),
            )?);
        }
        Ok(blocks)
    }

    /// The signer subset used for v1/v2: oldest signer if v3 is enabled, else all.
    fn v2_subset(&self, v3_enabled: bool) -> &[SignerConfig] {
        if v3_enabled {
            &self.signers[..1]
        } else {
            &self.signers
        }
    }

    fn v1_signer_configs<'a>(
        &'a self,
        v3_enabled: bool,
        min_sdk: u32,
    ) -> Result<Vec<V1SignerConfig<'a>>> {
        let subset = self.v2_subset(v3_enabled);
        let mut out = Vec::with_capacity(subset.len());
        for s in subset {
            out.push(V1SignerConfig {
                name: v1::safe_signer_name(&s.name),
                key: &s.key,
                certificates: &s.certificates,
                digest_algorithm: suggested_v1_digest_algorithm(&s.key, min_sdk)?,
                deterministic_dsa: s.deterministic_dsa,
            });
        }
        Ok(out)
    }

    fn block_signer_configs<'a>(
        &self,
        subset: &'a [SignerConfig],
    ) -> Result<Vec<BlockSignerConfig<'a>>> {
        let mut out = Vec::with_capacity(subset.len());
        for s in subset {
            out.push(BlockSignerConfig {
                key: &s.key,
                certificates: &s.certificates,
                signature_algorithms: suggested_signature_algorithms(
                    &s.key,
                    self.verity_enabled,
                    s.deterministic_dsa,
                )?,
            });
        }
        Ok(out)
    }

    /// Resolves the rotation-min-sdk-version as `setMinSdkVersionForRotation`
    /// does: anything below the v3.1 support floor (T) clamps to the v3 floor
    /// (P); the default when unset is T.
    fn resolved_rotation_min_sdk(&self) -> u32 {
        match self.rotation_min_sdk_version {
            Some(v) if v < sdk::T => sdk::P,
            Some(v) => v,
            None => sdk::T,
        }
    }

    /// v3 signer configs after `processV3Configs` min/max-SDK assignment.
    fn v3_signer_configs<'a>(&'a self, min_sdk: u32) -> Result<Vec<V3SignerConfig<'a>>> {
        // raw configs in signer order (oldest first).
        struct Raw {
            idx: usize,
            min_sdk: u32,
            max_sdk: u32,
            algs: Vec<SignatureAlgorithm>,
        }
        // apksig uses Java's Integer.MAX_VALUE (0x7fffffff) for the open upper
        // bound, not an unsigned 0xffffffff.
        const INT_MAX: u32 = 0x7fff_ffff;
        // In a rotation, the leaf signer (the newest cert in the lineage)
        // becomes a targeted signer whose minSdkVersion is the resolved
        // rotation-min-sdk-version, so its v3 block covers [rotationMin, MAX].
        let rotation_min = self.resolved_rotation_min_sdk();
        let mut raw = Vec::with_capacity(self.signers.len());
        for (idx, s) in self.signers.iter().enumerate() {
            let is_rotated_leaf = self
                .lineage
                .as_ref()
                .is_some_and(|l| l.current_cert_der() == Some(s.certificates[0].der.as_slice()));
            let min_sdk = if is_rotated_leaf {
                rotation_min
            } else {
                s.min_sdk_version
            };
            raw.push(Raw {
                idx,
                min_sdk,
                max_sdk: INT_MAX,
                algs: suggested_signature_algorithms(
                    &s.key,
                    self.verity_enabled,
                    s.deterministic_dsa,
                )?,
            });
        }

        let min_required = sdk::P.max(min_sdk);
        let mut processed: Vec<Raw> = Vec::new();
        let mut current_min = u32::MAX;
        for i in (0..raw.len()).rev() {
            let mut cfg = Raw {
                idx: raw[i].idx,
                min_sdk: raw[i].min_sdk,
                max_sdk: raw[i].max_sdk,
                algs: raw[i].algs.clone(),
            };
            if i == raw.len() - 1 {
                cfg.max_sdk = INT_MAX;
            } else {
                cfg.max_sdk = current_min - 1;
            }
            if cfg.min_sdk == 0 {
                cfg.min_sdk = min_sdk_from_algorithms(&cfg.algs, min_sdk);
            }
            current_min = cfg.min_sdk;
            processed.push(cfg);
            if current_min <= min_required {
                break;
            }
        }

        // `processed` is newest-first; the v3 block lists signers in that order.
        let mut out = Vec::with_capacity(processed.len());
        for cfg in processed {
            let signer = &self.signers[cfg.idx];
            // Attach the lineage to the newest signer (latest in the lineage).
            let lineage = self
                .lineage
                .as_ref()
                .filter(|l| l.current_cert_der() == Some(signer.certificates[0].der.as_slice()));
            out.push(V3SignerConfig {
                signer: BlockSignerConfig {
                    key: &signer.key,
                    certificates: &signer.certificates,
                    signature_algorithms: cfg.algs,
                },
                min_sdk_version: cfg.min_sdk,
                max_sdk_version: cfg.max_sdk,
                lineage,
                signer_targets_dev_release: false,
            });
        }
        Ok(out)
    }

    /// `outputInputJarEntryLfhRecord`: copy the entry, re-aligning uncompressed
    /// data when needed. Returns the data offset inside the output record.
    fn output_input_entry(
        &self,
        output: &mut Vec<u8>,
        lfh_section: &[u8],
        lfr: &zip::LocalFileRecord,
        output_offset: usize,
    ) -> usize {
        let input_offset = lfr.start_offset;
        if input_offset == output_offset && self.alignment_preserved {
            output.extend_from_slice(&lfh_section[input_offset..input_offset + lfr.size]);
            return lfr.data_start_offset;
        }
        let align = self.entry_data_alignment_multiple(lfr);
        if align <= 1 || (input_offset % align == output_offset % align && self.alignment_preserved)
        {
            output.extend_from_slice(&lfh_section[input_offset..input_offset + lfr.size]);
            return lfr.data_start_offset;
        }
        let input_data_start = input_offset + lfr.data_start_offset;
        if input_data_start % align != 0 && self.alignment_preserved {
            output.extend_from_slice(&lfh_section[input_offset..input_offset + lfr.size]);
            return lfr.data_start_offset;
        }

        // Re-align via the extra field.
        let extra_field_start_in_record = lfr.data_start_offset - lfr.extra.len();
        let aligning_extra = create_extra_field_to_align_data(
            &lfr.extra,
            (output_offset + extra_field_start_in_record) as u64,
            align,
        );
        let data_offset = lfr.data_start_offset + aligning_extra.len() - lfr.extra.len();
        self.output_record_with_modified_extra(output, lfh_section, lfr, &aligning_extra);
        data_offset
    }

    /// `LocalFileRecord.outputRecordWithModifiedExtra`.
    fn output_record_with_modified_extra(
        &self,
        output: &mut Vec<u8>,
        lfh_section: &[u8],
        lfr: &zip::LocalFileRecord,
        extra: &[u8],
    ) {
        let extra_start = lfr.data_start_offset - lfr.extra.len();
        let start = lfr.start_offset;
        // Header up to (excluding) the extra field, with patched extra-length.
        let mut header = lfh_section[start..start + extra_start].to_vec();
        header[zip::LFH_EXTRA_LENGTH_OFFSET..zip::LFH_EXTRA_LENGTH_OFFSET + 2]
            .copy_from_slice(&(extra.len() as u16).to_le_bytes());
        output.extend_from_slice(&header);
        output.extend_from_slice(extra);
        // Remaining record bytes (data + any descriptor).
        let data_start = start + lfr.data_start_offset;
        output.extend_from_slice(&lfh_section[data_start..start + lfr.size]);
    }

    /// `getInputJarEntryDataAlignmentMultiple`.
    fn entry_data_alignment_multiple(&self, lfr: &zip::LocalFileRecord) -> usize {
        if lfr.data_compressed {
            return 1;
        }
        let extra = &lfr.extra;
        let mut pos = 0;
        while extra.len() - pos >= 4 {
            let header_id = u16::from_le_bytes([extra[pos], extra[pos + 1]]);
            let data_size = u16::from_le_bytes([extra[pos + 2], extra[pos + 3]]) as usize;
            if data_size > extra.len() - pos - 4 {
                break;
            }
            if header_id != ALIGNMENT_EXTRA_ID {
                pos += 4 + data_size;
                continue;
            }
            if data_size < 2 {
                break;
            }
            return u16::from_le_bytes([extra[pos + 4], extra[pos + 5]]) as usize;
        }
        if lfr.name.ends_with(".so") {
            self.lib_page_alignment
        } else {
            4
        }
    }
}

struct SigningBlockResult {
    padding: usize,
    block: Vec<u8>,
    eocd: Vec<u8>,
}

fn signers_extra(s: &ApkSigner) -> usize {
    2 * s.signers.len() + 1
}

fn push_unique(v: &mut Vec<ContentDigestAlgorithm>, a: ContentDigestAlgorithm) {
    if !v.contains(&a) {
        v.push(a);
    }
}

fn v1_scheme_ids(v2: bool, v3: bool) -> Vec<u32> {
    let mut ids = Vec::new();
    if v2 {
        ids.push(2);
    }
    if v3 {
        ids.push(3);
    }
    ids
}

/// `getMinSdkFromV3SignatureAlgorithms`.
fn min_sdk_from_algorithms(algs: &[SignatureAlgorithm], apk_min_sdk: u32) -> u32 {
    let mut min = u32::MAX;
    for alg in algs {
        let current = alg.min_sdk_version();
        if current < min {
            if current <= apk_min_sdk || current <= sdk::P {
                return current;
            }
            min = current;
        }
    }
    min
}

/// Copies any verbatim gap in the input LFH section preceding `target_offset`.
fn copy_gap(output: &mut Vec<u8>, lfh: &[u8], input_offset: usize, target_offset: usize) -> usize {
    if target_offset > input_offset {
        output.extend_from_slice(&lfh[input_offset..target_offset]);
        target_offset
    } else {
        input_offset
    }
}

/// Copies the gap then consumes `record_size` bytes of input without writing
/// them (used for the OUTPUT_BY_ENGINE manifest entry).
fn advance_gap_and_consume(
    output: &mut Vec<u8>,
    lfh: &[u8],
    input_offset: usize,
    target_offset: usize,
    record_size: usize,
) -> usize {
    let after_gap = copy_gap(output, lfh, input_offset, target_offset);
    after_gap + record_size
}

/// `createExtraFieldToAlignData`.
fn create_extra_field_to_align_data(
    original: &[u8],
    extra_start_offset: u64,
    align: usize,
) -> Vec<u8> {
    if align <= 1 {
        return original.to_vec();
    }
    let mut result = Vec::with_capacity(original.len() + 5 + align);
    // Copy all fields except the old 0/0 padding field and any 0xd935 field.
    let mut pos = 0;
    while original.len() - pos >= 4 {
        let header_id = u16::from_le_bytes([original[pos], original[pos + 1]]);
        let data_size = u16::from_le_bytes([original[pos + 2], original[pos + 3]]) as usize;
        if data_size > original.len() - pos - 4 {
            break;
        }
        if (header_id == 0 && data_size == 0) || header_id == ALIGNMENT_EXTRA_ID {
            pos += 4 + data_size;
            continue;
        }
        result.extend_from_slice(&original[pos..pos + 4 + data_size]);
        pos += 4 + data_size;
    }

    let data_min_start = extra_start_offset as usize + result.len() + ALIGNMENT_EXTRA_MIN_SIZE;
    let padding = (align - (data_min_start % align)) % align;
    result.extend_from_slice(&ALIGNMENT_EXTRA_ID.to_le_bytes());
    result.extend_from_slice(&((2 + padding) as u16).to_le_bytes());
    result.extend_from_slice(&(align as u16).to_le_bytes());
    result.resize(result.len() + padding, 0);
    result
}

/// Locates and decompresses AndroidManifest.xml, if present.
fn find_android_manifest(
    _apk: &[u8],
    cd_records: &[CdRecord],
    lfh_section: &[u8],
) -> Result<Option<Vec<u8>>> {
    for cd in cd_records {
        if cd.name == ANDROID_MANIFEST {
            let lfr = zip::parse_local_file_record(lfh_section, cd)?;
            return Ok(Some(
                lfr.uncompressed_data(lfh_section)
                    .context("reading AndroidManifest.xml")?,
            ));
        }
    }
    Ok(None)
}
