//! Content digests for the v2/v3/v4 signature schemes
//! (`ApkSigningBlockUtils.computeContentDigests` + `VerityTreeBuilder`).

use anyhow::{bail, Result};
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;

/// Content digest algorithms (`ContentDigestAlgorithm.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContentDigestAlgorithm {
    ChunkedSha256,
    ChunkedSha512,
    VerityChunkedSha256,
    /// Whole-file SHA-256 (source stamp only).
    Sha256,
}

impl ContentDigestAlgorithm {
    pub fn chunk_digest_output_size(self) -> usize {
        match self {
            ContentDigestAlgorithm::ChunkedSha256
            | ContentDigestAlgorithm::VerityChunkedSha256
            | ContentDigestAlgorithm::Sha256 => 32,
            ContentDigestAlgorithm::ChunkedSha512 => 64,
        }
    }
}

const ONE_MB: usize = 1024 * 1024;
const VERITY_CHUNK: usize = 4096;

/// Computes the requested content digests over the three APK sections.
///
/// `before_cd` must already include any signing-block padding, and `eocd`
/// must already have its CD-offset field pointed at the start of the (future)
/// signing block — see `copyWithModifiedCDOffset`.
pub fn compute_content_digests(
    algorithms: &[ContentDigestAlgorithm],
    before_cd: &[u8],
    central_dir: &[u8],
    eocd: &[u8],
) -> Result<BTreeMap<ContentDigestAlgorithm, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let chunked: Vec<ContentDigestAlgorithm> = algorithms
        .iter()
        .copied()
        .filter(|a| {
            matches!(
                a,
                ContentDigestAlgorithm::ChunkedSha256 | ContentDigestAlgorithm::ChunkedSha512
            )
        })
        .collect();
    if !chunked.is_empty() {
        let sections: [&[u8]; 3] = [before_cd, central_dir, eocd];
        for alg in chunked {
            out.insert(alg, one_mb_chunk_digest(alg, &sections));
        }
    }
    if algorithms.contains(&ContentDigestAlgorithm::VerityChunkedSha256) {
        if before_cd.len() % VERITY_CHUNK != 0 {
            bail!(
                "APK Signing Block size not a multiple of {VERITY_CHUNK}: {}",
                before_cd.len()
            );
        }
        // 32-byte root hash + uint64 LE total size, salt = 8 zero bytes.
        let salt = [0u8; 8];
        let root = verity_root_hash(&[before_cd, central_dir, eocd], Some(&salt));
        let total = (before_cd.len() + central_dir.len() + eocd.len()) as u64;
        let mut digest = Vec::with_capacity(40);
        digest.extend_from_slice(&root);
        digest.extend_from_slice(&total.to_le_bytes());
        out.insert(ContentDigestAlgorithm::VerityChunkedSha256, digest);
    }
    Ok(out)
}

/// 1 MB chunk digest: each chunk hashed as 0xa5 || len(u32 LE) || data; the
/// final digest hashes 0x5a || chunk-count(u32 LE) || chunk digests.
fn one_mb_chunk_digest(alg: ContentDigestAlgorithm, sections: &[&[u8]; 3]) -> Vec<u8> {
    let chunk_count: usize = sections.iter().map(|s| s.len().div_ceil(ONE_MB)).sum();
    let digest_size = alg.chunk_digest_output_size();
    let mut concat = Vec::with_capacity(5 + chunk_count * digest_size);
    concat.push(0x5a);
    concat.extend_from_slice(&(chunk_count as u32).to_le_bytes());
    for section in sections {
        for chunk in section.chunks(ONE_MB) {
            let mut prefix = [0xa5u8, 0, 0, 0, 0];
            prefix[1..5].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
            match alg {
                ContentDigestAlgorithm::ChunkedSha256 => {
                    let mut h = Sha256::new();
                    h.update(prefix);
                    h.update(chunk);
                    concat.extend_from_slice(&h.finalize());
                }
                ContentDigestAlgorithm::ChunkedSha512 => {
                    let mut h = Sha512::new();
                    h.update(prefix);
                    h.update(chunk);
                    concat.extend_from_slice(&h.finalize());
                }
                _ => unreachable!(),
            }
        }
    }
    match alg {
        ContentDigestAlgorithm::ChunkedSha256 => Sha256::digest(&concat).to_vec(),
        ContentDigestAlgorithm::ChunkedSha512 => Sha512::digest(&concat).to_vec(),
        _ => unreachable!(),
    }
}

/// Builds the full verity hash tree over `data` (`VerityTreeBuilder
/// .generateVerityTree`), returning the tree bytes. SHA-256, 4096-byte
/// chunks, bottom level first in computation but top level first in layout.
pub fn verity_tree(sections: &[&[u8]], salt: Option<&[u8]>) -> Vec<u8> {
    let digest_size = 32usize;
    let data_size: usize = sections.iter().map(|s| s.len()).sum();

    // Level sizes bottom-to-top, then convert to a top-first offset table.
    let mut level_sizes: Vec<usize> = Vec::new();
    let mut size = data_size as u64;
    loop {
        let chunk_count = size.div_ceil(VERITY_CHUNK as u64);
        let level = (VERITY_CHUNK as u64) * (chunk_count * digest_size as u64).div_ceil(VERITY_CHUNK as u64);
        level_sizes.push(level as usize);
        if chunk_count * digest_size as u64 <= VERITY_CHUNK as u64 {
            break;
        }
        size = chunk_count * digest_size as u64;
    }
    let mut level_offset = vec![0usize; level_sizes.len() + 1];
    for i in 0..level_sizes.len() {
        level_offset[i + 1] = level_offset[i] + level_sizes[level_sizes.len() - i - 1];
    }

    let mut tree = vec![0u8; level_offset[level_offset.len() - 1]];
    for i in (0..level_sizes.len()).rev() {
        let (out_start, out_end) = (level_offset[i], level_offset[i + 1]);
        let digests = if i == level_sizes.len() - 1 {
            digest_chunks(sections, salt)
        } else {
            let src = tree[level_offset[i + 1]..level_offset[i + 2]].to_vec();
            digest_chunks(&[&src], salt)
        };
        // Copy digests, zero-padding the rest of the level (buffer is
        // pre-zeroed, so only the copy is needed).
        tree[out_start..out_start + digests.len()].copy_from_slice(&digests);
        debug_assert!(digests.len() <= out_end - out_start);
    }
    tree
}

/// Digest of the first 4096-byte page of the tree, salted.
pub fn verity_root_from_tree(tree: &[u8], salt: Option<&[u8]>) -> Vec<u8> {
    let mut h = Sha256::new();
    if let Some(salt) = salt {
        h.update(salt);
    }
    let first_page = &tree[..VERITY_CHUNK.min(tree.len())];
    h.update(first_page);
    if first_page.len() < VERITY_CHUNK {
        h.update(vec![0u8; VERITY_CHUNK - first_page.len()]);
    }
    h.finalize().to_vec()
}

/// Convenience: builds the tree and returns the root hash.
pub fn verity_root_hash(sections: &[&[u8]], salt: Option<&[u8]>) -> Vec<u8> {
    let tree = verity_tree(sections, salt);
    verity_root_from_tree(&tree, salt)
}

/// Digests consecutive 4096-byte chunks across `sections` (zero-padding the
/// final partial chunk), returning the concatenated digests.
fn digest_chunks(sections: &[&[u8]], salt: Option<&[u8]>) -> Vec<u8> {
    let total: usize = sections.iter().map(|s| s.len()).sum();
    let chunks = total.div_ceil(VERITY_CHUNK);
    let mut out = Vec::with_capacity(chunks * 32);
    let mut chunk = Vec::with_capacity(VERITY_CHUNK);
    let mut emit = |chunk: &mut Vec<u8>| {
        chunk.resize(VERITY_CHUNK, 0);
        let mut h = Sha256::new();
        if let Some(salt) = salt {
            h.update(salt);
        }
        h.update(&*chunk);
        out.extend_from_slice(&h.finalize());
        chunk.clear();
    };
    for section in sections {
        let mut data = &section[..];
        while !data.is_empty() {
            let take = (VERITY_CHUNK - chunk.len()).min(data.len());
            chunk.extend_from_slice(&data[..take]);
            data = &data[take..];
            if chunk.len() == VERITY_CHUNK {
                emit(&mut chunk);
            }
        }
    }
    if !chunk.is_empty() {
        emit(&mut chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_sha256_small() {
        let digests = compute_content_digests(
            &[ContentDigestAlgorithm::ChunkedSha256],
            b"abc",
            b"def",
            b"ghi",
        )
        .unwrap();
        let d = &digests[&ContentDigestAlgorithm::ChunkedSha256];
        assert_eq!(d.len(), 32);
        // 3 chunks of 3 bytes each.
        let mut chunk_digests = Vec::new();
        for part in [b"abc", b"def", b"ghi"] {
            let mut h = Sha256::new();
            h.update([0xa5, 3, 0, 0, 0]);
            h.update(part);
            chunk_digests.extend_from_slice(&h.finalize());
        }
        let mut top = Sha256::new();
        top.update([0x5a, 3, 0, 0, 0]);
        top.update(&chunk_digests);
        assert_eq!(d.as_slice(), top.finalize().as_slice());
    }

    #[test]
    fn verity_single_level() {
        // Data smaller than one page: tree is a single page of one digest.
        let data = vec![7u8; 100];
        let tree = verity_tree(&[&data], None);
        assert_eq!(tree.len(), 4096);
        let mut padded = data.clone();
        padded.resize(4096, 0);
        let expected = Sha256::digest(&padded);
        assert_eq!(&tree[..32], expected.as_slice());
        assert!(tree[32..].iter().all(|&b| b == 0));
    }

    #[test]
    fn verity_two_levels() {
        // > 128 chunks of data forces two levels.
        let data = vec![1u8; 4096 * 200];
        let tree = verity_tree(&[&data], None);
        // Bottom level: 200 digests * 32 = 6400 -> 8192 bytes; top: 4096.
        assert_eq!(tree.len(), 4096 + 8192);
        let root = verity_root_from_tree(&tree, None);
        assert_eq!(root.len(), 32);
    }
}
