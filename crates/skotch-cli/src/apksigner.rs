//! `skotch apksigner` — a drop-in reimplementation of the Android SDK
//! `apksigner` CLI on top of [`skotch_apksig`].
//!
//! Mirrors the upstream tool's option grammar (`--name value` /
//! `--name=value`, optional boolean values, `pass:`/`env:`/`file:`/`stdin`
//! password specs) and the `sign` / `verify` / `rotate` / `lineage` /
//! `version` / `help` commands. Signing and verification run entirely
//! in-process; the same APIs back the skotch build pipeline.

use anyhow::{anyhow, bail, Context, Result};
use skotch_apksig::{
    keystore, ApkSigner, ApkVerifier, Certificate, PrivateKey, SignerCapabilities, SignerConfig,
    SigningCertificateLineage,
};
use std::path::Path;

const VERSION: &str = "0.9-skotch";

const HELP: &str = include_str!("apksigner_help/help.txt");
const HELP_SIGN: &str = include_str!("apksigner_help/help_sign.txt");
const HELP_VERIFY: &str = include_str!("apksigner_help/help_verify.txt");
const HELP_ROTATE: &str = include_str!("apksigner_help/help_rotate.txt");
const HELP_LINEAGE: &str = include_str!("apksigner_help/help_lineage.txt");

/// Entry point for the `apksigner` subcommand.
pub fn run(args: &[String]) -> Result<()> {
    let cmd = args.first().map(String::as_str);
    match cmd {
        None | Some("--help") | Some("-h") => {
            print!("{HELP}");
            Ok(())
        }
        Some("--version") | Some("version") => {
            println!("{VERSION}");
            Ok(())
        }
        Some("help") => {
            print!("{HELP}");
            Ok(())
        }
        Some("sign") => sign(&args[1..]),
        Some("verify") => verify(&args[1..]),
        Some("rotate") => rotate(&args[1..]),
        Some("lineage") => lineage(&args[1..]),
        Some(other) => bail!("Unsupported command: {other}. See --help for supported commands"),
    }
}

// ── Option parser (OptionsParser.java) ────────────────────────────────────

struct OptionsParser {
    params: Vec<String>,
    index: usize,
    last_value: Option<String>,
    last_original: String,
}

impl OptionsParser {
    fn new(params: &[String]) -> OptionsParser {
        OptionsParser {
            params: params.to_vec(),
            index: 0,
            last_value: None,
            last_original: String::new(),
        }
    }

    /// Returns the next option name (without leading dashes), or `None` at the
    /// end of options (`--` or a non-option positional).
    fn next_option(&mut self) -> Option<String> {
        if self.index >= self.params.len() {
            return None;
        }
        let param = self.params[self.index].clone();
        if !param.starts_with('-') {
            return None;
        }
        self.index += 1;
        self.last_value = None;
        if let Some(stripped) = param.strip_prefix("--") {
            if stripped.is_empty() {
                return None; // bare "--"
            }
            if let Some(eq) = stripped.find('=') {
                self.last_value = Some(stripped[eq + 1..].to_string());
                self.last_original = format!("--{}", &stripped[..eq]);
                return Some(stripped[..eq].to_string());
            }
            self.last_original = param.clone();
            Some(stripped.to_string())
        } else {
            let name = &param[1..];
            self.last_original = param.clone();
            Some(name.to_string())
        }
    }

    fn required_value(&mut self, desc: &str) -> Result<String> {
        if let Some(v) = self.last_value.take() {
            return Ok(v);
        }
        if self.index >= self.params.len() || self.params[self.index] == "--" {
            bail!("{desc} missing after {}", self.last_original);
        }
        let v = self.params[self.index].clone();
        self.index += 1;
        Ok(v)
    }

    fn required_int(&mut self, desc: &str) -> Result<u32> {
        let v = self.required_value(desc)?;
        v.parse::<u32>()
            .map_err(|_| anyhow!("{desc} ({}) must be a decimal number: {v}", self.last_original))
    }

    /// Optional boolean: `--flag` → `default`; `--flag=true/false` or a
    /// following `true`/`false` token sets it explicitly.
    fn optional_boolean(&mut self, default: bool) -> Result<bool> {
        if let Some(v) = self.last_value.take() {
            return match v.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => bail!(
                    "Unsupported value for {}: {other}. Only true or false supported.",
                    self.last_original
                ),
            };
        }
        if self.index < self.params.len() {
            match self.params[self.index].as_str() {
                "true" => {
                    self.index += 1;
                    return Ok(true);
                }
                "false" => {
                    self.index += 1;
                    return Ok(false);
                }
                _ => {}
            }
        }
        Ok(default)
    }

    /// Remaining positional parameters (after an optional leading `--`).
    fn remaining(&mut self) -> Vec<String> {
        if self.index < self.params.len() && self.params[self.index] == "--" {
            self.index += 1;
        }
        self.params[self.index..].to_vec()
    }
}

// ── Password specs (PasswordRetriever) ────────────────────────────────────

fn resolve_password(spec: &str, prompt: &str) -> Result<String> {
    if let Some(p) = spec.strip_prefix("pass:") {
        Ok(p.to_string())
    } else if let Some(var) = spec.strip_prefix("env:") {
        std::env::var(var)
            .map_err(|_| anyhow!("Failed to read {prompt}: environment variable {var} not specified"))
    } else if let Some(path) = spec.strip_prefix("file:") {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {prompt} from {path}"))?;
        Ok(content.lines().next().unwrap_or("").to_string())
    } else if spec == "stdin" {
        use std::io::BufRead;
        eprint!("{prompt}: ");
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    } else {
        bail!("Unsupported password spec for {prompt}: {spec}")
    }
}

// ── Signer parameters ─────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct SignerParams {
    ks: Option<String>,
    ks_key_alias: Option<String>,
    ks_pass: Option<String>,
    key_pass: Option<String>,
    ks_type: Option<String>,
    key: Option<String>,
    cert: Option<String>,
    v1_signer_name: Option<String>,
    // capability overrides for rotate/lineage
    set_installed_data: Option<bool>,
    set_shared_uid: Option<bool>,
    set_permission: Option<bool>,
    set_rollback: Option<bool>,
    set_auth: Option<bool>,
}

impl SignerParams {
    fn is_empty(&self) -> bool {
        self.ks.is_none() && self.key.is_none() && self.cert.is_none()
    }

    /// Tries to handle a signer-scoped option; returns true if consumed.
    fn handle(&mut self, name: &str, parser: &mut OptionsParser) -> Result<bool> {
        match name {
            "ks" => self.ks = Some(parser.required_value("KeyStore file")?),
            "ks-key-alias" => self.ks_key_alias = Some(parser.required_value("KeyStore key alias")?),
            "ks-pass" => self.ks_pass = Some(parser.required_value("KeyStore password")?),
            "key-pass" => self.key_pass = Some(parser.required_value("Key password")?),
            "ks-type" => self.ks_type = Some(parser.required_value("KeyStore type")?),
            "key" => self.key = Some(parser.required_value("Private key file")?),
            "cert" => self.cert = Some(parser.required_value("Certificate file")?),
            "v1-signer-name" => {
                self.v1_signer_name = Some(parser.required_value("V1 signer name")?)
            }
            "set-installed-data" => self.set_installed_data = Some(parser.optional_boolean(true)?),
            "set-shared-uid" => self.set_shared_uid = Some(parser.optional_boolean(true)?),
            "set-permission" => self.set_permission = Some(parser.optional_boolean(true)?),
            "set-rollback" => self.set_rollback = Some(parser.optional_boolean(true)?),
            "set-auth" => self.set_auth = Some(parser.optional_boolean(true)?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn capabilities(&self) -> Option<SignerCapabilities> {
        if self.set_installed_data.is_none()
            && self.set_shared_uid.is_none()
            && self.set_permission.is_none()
            && self.set_rollback.is_none()
            && self.set_auth.is_none()
        {
            return None;
        }
        let mut caps = SignerCapabilities::default_flags();
        use skotch_apksig::lineage::{
            PAST_CERT_AUTH, PAST_CERT_INSTALLED_DATA, PAST_CERT_PERMISSION, PAST_CERT_ROLLBACK,
            PAST_CERT_SHARED_USER_ID,
        };
        if let Some(v) = self.set_installed_data {
            caps.set(PAST_CERT_INSTALLED_DATA, v);
        }
        if let Some(v) = self.set_shared_uid {
            caps.set(PAST_CERT_SHARED_USER_ID, v);
        }
        if let Some(v) = self.set_permission {
            caps.set(PAST_CERT_PERMISSION, v);
        }
        if let Some(v) = self.set_rollback {
            caps.set(PAST_CERT_ROLLBACK, v);
        }
        if let Some(v) = self.set_auth {
            caps.set(PAST_CERT_AUTH, v);
        }
        Some(caps)
    }

    /// Loads the key + certificate chain and resolves the v1 signer name.
    fn load(&self, default_name: &str) -> Result<SignerConfig> {
        if let Some(ks_path) = &self.ks {
            let data = std::fs::read(ks_path).with_context(|| format!("reading {ks_path}"))?;
            let store_pass = match &self.ks_pass {
                Some(spec) => resolve_password(spec, "Keystore password")?,
                None => resolve_password("stdin", &format!("Keystore password for {ks_path}"))?,
            };
            let key_pass = match &self.key_pass {
                Some(spec) => Some(resolve_password(spec, "Key password")?),
                None => None,
            };
            let entry = keystore::load(
                &data,
                &store_pass,
                key_pass.as_deref(),
                self.ks_key_alias.as_deref(),
            )?;
            let name = self
                .v1_signer_name
                .clone()
                .or_else(|| self.ks_key_alias.clone())
                .or(entry.alias.clone())
                .unwrap_or_else(|| default_name.to_string());
            return Ok(SignerConfig {
                name,
                key: entry.key,
                certificates: entry.certificates,
                min_sdk_version: 0,
                deterministic_dsa: false,
            });
        }

        // --key + --cert path.
        let key_path = self.key.as_ref().context(
            "KeyStore (--ks) or private key file (--key) must be specified",
        )?;
        let cert_path = self
            .cert
            .as_ref()
            .context("Certificate file (--cert) must be specified")?;
        let key_data = std::fs::read(key_path).with_context(|| format!("reading {key_path}"))?;
        let key = PrivateKey::from_pkcs8_pem_or_der(&key_data)
            .with_context(|| format!("parsing private key {key_path}"))?;
        let cert_data = std::fs::read(cert_path).with_context(|| format!("reading {cert_path}"))?;
        let cert = Certificate::from_pem_or_der(&cert_data)
            .with_context(|| format!("parsing certificate {cert_path}"))?;
        let name = self
            .v1_signer_name
            .clone()
            .or_else(|| key_file_stem(key_path))
            .unwrap_or_else(|| default_name.to_string());
        Ok(SignerConfig {
            name,
            key,
            certificates: vec![cert],
            min_sdk_version: 0,
            deterministic_dsa: false,
        })
    }
}

fn key_file_stem(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.split('.').next().unwrap_or(n).to_string())
}

// ── sign ──────────────────────────────────────────────────────────────────

fn sign(args: &[String]) -> Result<()> {
    let mut parser = OptionsParser::new(args);
    let mut signers: Vec<SignerParams> = Vec::new();
    let mut current = SignerParams::default();

    let mut in_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut min_sdk: Option<u32> = None;
    let mut min_sdk_specified = false;
    let mut max_sdk: Option<u32> = None;
    let mut v1: Option<bool> = None;
    let mut v2: Option<bool> = None;
    let mut v3: Option<bool> = None;
    let mut v4: Option<bool> = None;
    let mut verity = false;
    let mut align_file_size = false;
    let mut alignment_preserved = false;
    let mut lib_page_alignment: Option<u32> = None;
    let mut debuggable_permitted = true;
    let mut deterministic_dsa = false;
    let mut verbose = false;
    let mut lineage_path: Option<String> = None;
    let mut rotation_min_sdk: Option<u32> = None;

    while let Some(name) = parser.next_option() {
        match name.as_str() {
            "help" | "h" => {
                print!("{HELP_SIGN}");
                return Ok(());
            }
            "in" => in_path = Some(parser.required_value("Input file")?),
            "out" => out_path = Some(parser.required_value("Output file")?),
            "min-sdk-version" => {
                min_sdk = Some(parser.required_int("Mininum API Level")?);
                min_sdk_specified = true;
            }
            "max-sdk-version" => max_sdk = Some(parser.required_int("Maximum API Level")?),
            "v1-signing-enabled" => v1 = Some(parser.optional_boolean(true)?),
            "v2-signing-enabled" => v2 = Some(parser.optional_boolean(true)?),
            "v3-signing-enabled" => v3 = Some(parser.optional_boolean(true)?),
            "v4-signing-enabled" => v4 = Some(parser.optional_boolean(true)?),
            "verity-enabled" => verity = parser.optional_boolean(true)?,
            "align-file-size" => align_file_size = true,
            "alignment-preserved" => alignment_preserved = parser.optional_boolean(true)?,
            "lib-page-alignment" => {
                lib_page_alignment = Some(parser.required_int("Library page alignment")?)
            }
            "debuggable-apk-permitted" => debuggable_permitted = parser.optional_boolean(true)?,
            "deterministic-dsa-signing" => deterministic_dsa = parser.optional_boolean(true)?,
            "verbose" | "v" => verbose = parser.optional_boolean(true)?,
            "lineage" => lineage_path = Some(parser.required_value("Lineage file")?),
            "rotation-min-sdk-version" => {
                rotation_min_sdk = Some(parser.required_int("Rotation min API Level")?)
            }
            "next-signer" => {
                if !current.is_empty() {
                    signers.push(std::mem::take(&mut current));
                }
            }
            other => {
                if !current.handle(other, &mut parser)? {
                    bail!("Unsupported option: --{other}. See --help for supported options.");
                }
            }
        }
    }
    if !current.is_empty() {
        signers.push(current);
    }
    if signers.is_empty() {
        bail!("At least one signer must be specified");
    }

    // Resolve input/output.
    let positionals = parser.remaining();
    if in_path.is_none() {
        match positionals.len() {
            0 => bail!("Missing input APK"),
            1 => in_path = Some(positionals[0].clone()),
            _ => bail!("Unexpected parameter(s) after input APK ({})", positionals[1]),
        }
    } else if !positionals.is_empty() {
        bail!("Unexpected parameter(s) after options: {}", positionals[0]);
    }
    let in_path = in_path.unwrap();
    let out_path = out_path.unwrap_or_else(|| in_path.clone());

    if min_sdk_specified {
        if let (Some(mn), Some(mx)) = (min_sdk, max_sdk) {
            if mn > mx {
                bail!("Min API Level ({mn}) > max API Level ({mx})");
            }
        }
    }

    // Load signers.
    let mut signer_configs = Vec::with_capacity(signers.len());
    for (i, sp) in signers.iter().enumerate() {
        let mut cfg = sp
            .load("CERT")
            .with_context(|| format!("Failed to load signer #{}", i + 1))?;
        cfg.deterministic_dsa = deterministic_dsa;
        signer_configs.push(cfg);
    }

    let lineage = match &lineage_path {
        Some(p) => Some(load_lineage(p)?),
        None => None,
    };

    let mut builder = ApkSigner::new(signer_configs)
        .verity_enabled(verity)
        .align_file_size(align_file_size)
        .alignment_preserved(alignment_preserved)
        .debuggable_apk_permitted(debuggable_permitted);
    if let Some(v) = v1 {
        builder = builder.v1_signing_enabled(v);
    }
    if let Some(v) = v2 {
        builder = builder.v2_signing_enabled(v);
    }
    if let Some(v) = v3 {
        builder = builder.v3_signing_enabled(v);
    }
    builder = builder.v4_signing_enabled(v4.unwrap_or(false));
    if let Some(s) = min_sdk {
        builder = builder.min_sdk_version(s);
    }
    if let Some(s) = rotation_min_sdk {
        builder = builder.rotation_min_sdk_version(s);
    }
    if let Some(a) = lib_page_alignment {
        builder = builder.lib_page_alignment(a as usize);
    }
    if let Some(l) = lineage {
        builder = builder.signing_certificate_lineage(l);
    }

    let input = std::fs::read(&in_path).with_context(|| format!("reading {in_path}"))?;
    let result = builder.sign(&input).context("signing APK")?;
    std::fs::write(&out_path, &result.apk).with_context(|| format!("writing {out_path}"))?;
    if let Some(idsig) = result.v4_signature {
        let idsig_path = format!("{out_path}.idsig");
        std::fs::write(&idsig_path, idsig).with_context(|| format!("writing {idsig_path}"))?;
    }
    if verbose {
        println!("Signed");
    }
    Ok(())
}

// ── verify ────────────────────────────────────────────────────────────────

fn verify(args: &[String]) -> Result<()> {
    let mut parser = OptionsParser::new(args);
    let mut in_path: Option<String> = None;
    let mut print_certs = false;
    let mut verbose = false;
    let mut werr = false;
    let mut min_sdk: Option<u32> = None;
    let mut max_sdk: Option<u32> = None;
    let mut v4_sig_file: Option<String> = None;

    while let Some(name) = parser.next_option() {
        match name.as_str() {
            "help" | "h" => {
                print!("{HELP_VERIFY}");
                return Ok(());
            }
            "in" => in_path = Some(parser.required_value("Input APK")?),
            "print-certs" => print_certs = parser.optional_boolean(true)?,
            "print-certs-pem" => print_certs = parser.optional_boolean(true)?,
            "verbose" | "v" => verbose = parser.optional_boolean(true)?,
            "Werr" => werr = parser.optional_boolean(true)?,
            "min-sdk-version" => min_sdk = Some(parser.required_int("Minimum API Level")?),
            "max-sdk-version" => max_sdk = Some(parser.required_int("Maximum API Level")?),
            "v4-signature-file" => v4_sig_file = Some(parser.required_value("V4 signature file")?),
            other => bail!("Unsupported option: --{other}. See --help for supported options."),
        }
    }
    let positionals = parser.remaining();
    if in_path.is_none() {
        match positionals.len() {
            0 => bail!("Missing APK"),
            1 => in_path = Some(positionals[0].clone()),
            _ => bail!("Unexpected parameter(s) after APK ({})", positionals[1]),
        }
    }
    let in_path = in_path.unwrap();

    let mut verifier = ApkVerifier::new();
    if let Some(s) = min_sdk {
        verifier = verifier.min_sdk_version(s);
    }
    if let Some(s) = max_sdk {
        verifier = verifier.max_sdk_version(s);
    }
    let input = std::fs::read(&in_path).with_context(|| format!("reading {in_path}"))?;
    let mut result = verifier.verify(&input)?;

    if let Some(v4_path) = &v4_sig_file {
        let idsig = std::fs::read(v4_path).with_context(|| format!("reading {v4_path}"))?;
        result.verified_v4 = verifier.verify_v4(&input, &idsig).unwrap_or(false);
    }

    if result.verified {
        if verbose {
            println!("Verifies");
            println!(
                "Verified using v1 scheme (JAR signing): {}",
                result.verified_v1
            );
            println!(
                "Verified using v2 scheme (APK Signature Scheme v2): {}",
                result.verified_v2
            );
            println!(
                "Verified using v3 scheme (APK Signature Scheme v3): {}",
                result.verified_v3
            );
            println!(
                "Verified using v3.1 scheme (APK Signature Scheme v3.1): {}",
                result.verified_v31
            );
            println!(
                "Verified using v4 scheme (APK Signature Scheme v4): {}",
                result.verified_v4
            );
            println!("Number of signers: {}", result.signer_certs.len());
        }
    } else {
        for e in &result.errors {
            eprintln!("ERROR: {e}");
        }
        eprintln!("DOES NOT VERIFY");
        std::process::exit(1);
    }

    if print_certs {
        for (i, signer) in result.signer_certs.iter().enumerate() {
            let cert = Certificate::from_der(&signer.cert_der)?;
            println!("Signer #{} certificate DN: {}", i + 1, cert.subject_rfc2253());
            println!(
                "Signer #{} certificate SHA-256 digest: {}",
                i + 1,
                hex(&sha256(&signer.cert_der))
            );
            println!(
                "Signer #{} certificate SHA-1 digest: {}",
                i + 1,
                hex(&sha1(&signer.cert_der))
            );
        }
    }

    for w in &result.warnings {
        if werr {
            eprintln!("ERROR: {w}");
        } else {
            println!("WARNING: {w}");
        }
    }
    if werr && !result.warnings.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

// ── rotate ────────────────────────────────────────────────────────────────

fn rotate(args: &[String]) -> Result<()> {
    let mut parser = OptionsParser::new(args);
    let mut in_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut old_signer = SignerParams::default();
    let mut new_signer = SignerParams::default();
    let mut min_sdk: u32 = skotch_apksig::sdk::P;
    let mut verbose = false;
    let mut target: Option<u8> = None; // 0 = old, 1 = new

    while let Some(name) = parser.next_option() {
        match name.as_str() {
            "help" | "h" => {
                print!("{HELP_ROTATE}");
                return Ok(());
            }
            "in" => in_path = Some(parser.required_value("Input lineage")?),
            "out" => out_path = Some(parser.required_value("Output lineage")?),
            "old-signer" => target = Some(0),
            "new-signer" => target = Some(1),
            "min-sdk-version" => min_sdk = parser.required_int("Minimum API Level")?,
            "verbose" | "v" => verbose = parser.optional_boolean(true)?,
            other => {
                let sp = match target {
                    Some(0) => &mut old_signer,
                    Some(1) => &mut new_signer,
                    _ => bail!("Unsupported option: --{other}"),
                };
                if !sp.handle(other, &mut parser)? {
                    bail!("Unsupported option: --{other}");
                }
            }
        }
    }
    if old_signer.is_empty() {
        bail!("Signer parameters for old signer not present");
    }
    if new_signer.is_empty() {
        bail!("Signer parameters for new signer not present");
    }
    let out_path = out_path.context("Output lineage file parameter not present")?;

    let old = old_signer.load("old")?;
    let new = new_signer.load("new")?;
    let caps = new_signer
        .capabilities()
        .unwrap_or_else(SignerCapabilities::default_flags);

    let lineage = match &in_path {
        Some(p) => {
            let existing = load_lineage(p)?;
            existing.spawn_descendant(&old.key, &old.certificates[0], &new.certificates[0], caps)?
        }
        None => SigningCertificateLineage::create(
            &old.key,
            &old.certificates[0],
            &new.certificates[0],
            caps,
            min_sdk,
        )?,
    };
    std::fs::write(&out_path, lineage.to_bytes())
        .with_context(|| format!("writing {out_path}"))?;
    if verbose {
        println!("Rotation entry generated.");
    }
    Ok(())
}

// ── lineage ───────────────────────────────────────────────────────────────

fn lineage(args: &[String]) -> Result<()> {
    let mut parser = OptionsParser::new(args);
    let mut in_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut print_certs = false;
    let mut verbose = false;

    while let Some(name) = parser.next_option() {
        match name.as_str() {
            "help" | "h" => {
                print!("{HELP_LINEAGE}");
                return Ok(());
            }
            "in" => in_path = Some(parser.required_value("Input lineage")?),
            "out" => out_path = Some(parser.required_value("Output lineage")?),
            "print-certs" => print_certs = parser.optional_boolean(true)?,
            "print-certs-pem" => print_certs = parser.optional_boolean(true)?,
            "verbose" | "v" => verbose = parser.optional_boolean(true)?,
            "signer" => { /* capability edits not yet supported; parsed away */ }
            other => bail!("Unsupported option: --{other}"),
        }
    }
    let _ = (out_path, verbose);
    let in_path = in_path.context("Input lineage file (--in) must be specified")?;
    let lineage = load_lineage(&in_path)?;

    if print_certs {
        for (i, node) in lineage.nodes.iter().enumerate() {
            let cert = Certificate::from_der(&node.signing_cert)?;
            println!("Signer #{} in lineage", i + 1);
            println!("Signer #{} certificate DN: {}", i + 1, cert.subject_rfc2253());
            println!(
                "Signer #{} certificate SHA-256 digest: {}",
                i + 1,
                hex(&sha256(&node.signing_cert))
            );
            let caps = SignerCapabilities::from_flags(node.flags);
            println!("Has installed data capability: {}", caps.has_installed_data());
            println!("Has shared UID capability    : {}", caps.has_shared_uid());
            println!("Has permission capability    : {}", caps.has_permission());
            println!("Has rollback capability      : {}", caps.has_rollback());
            println!("Has auth capability          : {}", caps.has_auth());
        }
    }
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────

fn load_lineage(path: &str) -> Result<SigningCertificateLineage> {
    let data = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    SigningCertificateLineage::from_bytes(&data)
        .map_err(|_| anyhow!("The input file is not a valid lineage file."))
}

fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).to_vec()
}

fn sha1(data: &[u8]) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    Sha1::digest(data).to_vec()
}

fn hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn name_value_and_name_eq_value() {
        let mut p = OptionsParser::new(&args(&["--out", "x.apk", "--in=y.apk"]));
        assert_eq!(p.next_option().as_deref(), Some("out"));
        assert_eq!(p.required_value("out").unwrap(), "x.apk");
        assert_eq!(p.next_option().as_deref(), Some("in"));
        assert_eq!(p.required_value("in").unwrap(), "y.apk");
        assert!(p.next_option().is_none());
    }

    #[test]
    fn optional_boolean_default_and_lookahead() {
        // Bare flag → default.
        let mut p = OptionsParser::new(&args(&["--v2-signing-enabled"]));
        p.next_option();
        assert!(p.optional_boolean(true).unwrap());

        // Following true/false token is consumed.
        let mut p = OptionsParser::new(&args(&["--v2-signing-enabled", "false", "leftover"]));
        p.next_option();
        assert!(!p.optional_boolean(true).unwrap());
        assert_eq!(p.remaining(), vec!["leftover".to_string()]);

        // Explicit =value.
        let mut p = OptionsParser::new(&args(&["--v2-signing-enabled=false"]));
        p.next_option();
        assert!(!p.optional_boolean(true).unwrap());
    }

    #[test]
    fn short_flags_and_double_dash_terminates() {
        let mut p = OptionsParser::new(&args(&["-v", "--", "input.apk"]));
        assert_eq!(p.next_option().as_deref(), Some("v"));
        assert!(!p.optional_boolean(false).unwrap()); // no following true/false
        // `--` ends options.
        assert!(p.next_option().is_none());
        assert_eq!(p.remaining(), vec!["input.apk".to_string()]);
    }

    #[test]
    fn positional_after_options() {
        let mut p = OptionsParser::new(&args(&["--out", "o.apk", "in.apk"]));
        assert_eq!(p.next_option().as_deref(), Some("out"));
        p.required_value("out").unwrap();
        assert!(p.next_option().is_none());
        assert_eq!(p.remaining(), vec!["in.apk".to_string()]);
    }

    #[test]
    fn password_specs() {
        assert_eq!(resolve_password("pass:secret", "x").unwrap(), "secret");
        std::env::set_var("APKSIG_TEST_PW", "envpw");
        assert_eq!(resolve_password("env:APKSIG_TEST_PW", "x").unwrap(), "envpw");
        assert!(resolve_password("bogus:foo", "x").is_err());
    }
}
