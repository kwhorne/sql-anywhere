//! Signing and verification for the SQL Anywhere extension repository.
//!
//! Loadable extensions are shipped as release archives, which means a user
//! downloads a shared object from the network and hands it to `.load`, where it
//! runs with the full privileges of the host process. Integrity and
//! authenticity therefore have to be checkable *before* the file is loaded.
//!
//! The scheme is deliberately small and inspectable:
//!
//! * A release directory of artifacts is described by a `MANIFEST.json`, which
//!   records each artifact's SHA-256 and the extension interface version it was
//!   built against (see `SQLANYWHERE_API_VERSION` in `sqlite3ext.h`).
//! * `MANIFEST.json.sig` is a detached Ed25519 signature over the exact bytes
//!   of that file, so verification never depends on re-serialising JSON the
//!   same way.
//! * `SHA256SUMS` is emitted alongside so that plain `sha256sum -c` gives
//!   anyone an integrity check without this tool.
//!
//! Keys carry a short id derived from the public key, so a signature names the
//! key that made it and rotation does not need a flag day.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PUBKEY_TAG: &str = "sqlanywhere-ext-pubkey-v1";
const SECKEY_TAG: &str = "sqlanywhere-ext-seckey-v1";
const SIG_TAG: &str = "sqlanywhere-ext-sig-v1";

const MANIFEST_NAME: &str = "MANIFEST.json";
const SIG_NAME: &str = "MANIFEST.json.sig";
const SUMS_NAME: &str = "SHA256SUMS";

/// Committed trust root. The private half never leaves the maintainer's
/// keychain and the CI secret.
pub const DEFAULT_PUBKEY_PATH: &str = "sqlanywhere-sqlite3/ext/SIGNING_KEY.pub";

const SECRET_FILE: &str = "SIGNING_KEY.secret";

/// Where the private half goes by default: the user's config directory, never
/// a working tree. A key inside a checkout is one `git add -f`, one stray
/// archive, or one `rsync` away from being published, and the whole point of
/// the trust root is that only one copy of it exists.
fn default_secret_path() -> Result<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.trim().is_empty() {
            return Ok(PathBuf::from(x).join("sqlanywhere").join(SECRET_FILE));
        }
    }
    let home = std::env::var("HOME")
        .context("neither XDG_CONFIG_HOME nor HOME is set; pass --secret PATH")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("sqlanywhere")
        .join(SECRET_FILE))
}

/// The working tree we are standing in, if any.
fn repo_root() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let path = PathBuf::from(text.trim());
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Absolute form of a path that does not exist yet: canonicalise the deepest
/// ancestor that does exist, then re-attach the rest. Without this, a symlinked
/// home directory would defeat the containment check below.
fn absolutise(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut cur = abs.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if cur.exists() {
            let mut base = cur.canonicalize().unwrap_or(cur);
            while let Some(t) = tail.pop() {
                base.push(t);
            }
            return base;
        }
        match (cur.file_name().map(|n| n.to_os_string()), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent.to_path_buf();
            }
            _ => return abs,
        }
    }
}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

// ---------------------------------------------------------------------------
// manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version, so a client can refuse what it cannot read.
    pub schema: u32,
    /// Release this manifest describes, e.g. `v0.5.3`.
    pub release: String,
    /// Key id expected to have signed this manifest, or `null` when the
    /// release was built without a signing key available.
    pub signing_key: Option<String>,
    pub extensions: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Extension name, e.g. `crsqlite`.
    pub name: String,
    /// Rust target triple the artifact was built for.
    pub target: String,
    /// File name inside the release.
    pub file: String,
    pub size: u64,
    pub sha256: String,
    /// Value of `SQLANYWHERE_API_VERSION` this artifact was compiled against.
    /// A host implementing a lower interface version should refuse the file
    /// rather than load it and hope.
    pub sqlanywhere_api_version: u32,
}

/// Read `SQLANYWHERE_API_VERSION` out of a `sqlite3ext.h`.
pub fn api_version_from_header(header: &Path) -> Result<u32> {
    let text =
        fs::read_to_string(header).with_context(|| format!("reading {}", header.display()))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("#define SQLANYWHERE_API_VERSION") {
            return rest
                .trim()
                .parse()
                .with_context(|| format!("parsing SQLANYWHERE_API_VERSION from {line:?}"));
        }
    }
    bail!(
        "{} does not define SQLANYWHERE_API_VERSION",
        header.display()
    )
}

/// First `sqlite3ext.h` among the usual locations, so this works both in a
/// built tree and from a bare checkout.
pub fn find_header() -> Result<PathBuf> {
    let candidates = [
        "sqlanywhere-sqlite3/sqlite3ext.h",
        "sqlanywhere-sqlite3/src/sqlite3ext.h",
        "sqlanywhere-ffi/bundled/src/sqlite3ext.h",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("no sqlite3ext.h found among {candidates:?}")
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok((hex::encode(h.finalize()), bytes.len() as u64))
}

/// `crsqlite-v0.5.3-aarch64-apple-darwin.tar.gz` -> ("crsqlite", target).
///
/// Falls back to the whole stem as the name when the shape is unfamiliar, so an
/// unexpected artifact is still listed rather than silently dropped.
fn split_artifact_name(file: &str, release: &str) -> (String, String) {
    let stem = file
        .strip_suffix(".tar.gz")
        .or_else(|| file.strip_suffix(".zip"))
        .or_else(|| file.strip_suffix(".tgz"))
        .unwrap_or(file);

    let marker = format!("-{release}-");
    if let Some(pos) = stem.find(&marker) {
        let name = &stem[..pos];
        let target = &stem[pos + marker.len()..];
        if !name.is_empty() && !target.is_empty() {
            return (name.to_string(), target.to_string());
        }
    }
    (stem.to_string(), "unknown".to_string())
}

// ---------------------------------------------------------------------------
// keys
// ---------------------------------------------------------------------------

fn key_id(vk: &VerifyingKey) -> String {
    let mut h = Sha256::new();
    h.update(vk.as_bytes());
    hex::encode(&h.finalize()[..4])
}

fn parse_tagged(text: &str, tag: &str, fields: usize) -> Result<Vec<String>> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .context("file has no content line")?;
    let parts: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    if parts.first().map(String::as_str) != Some(tag) {
        bail!("expected a line tagged {tag}, found {:?}", parts.first());
    }
    if parts.len() < fields + 1 {
        bail!(
            "{tag} line has {} fields, expected {}",
            parts.len() - 1,
            fields
        );
    }
    Ok(parts[1..].to_vec())
}

fn read_pubkey(path: &Path) -> Result<(VerifyingKey, String)> {
    if !path.is_file() {
        bail!(
            "no trusted public key at {}.\n\
             This tree has no extension trust root yet. Create one once with\n\
             `cargo xtask extension-keygen`, commit the resulting SIGNING_KEY.pub,\n\
             and store the secret line as the CI secret EXTENSION_SIGNING_KEY.\n\
             To check digests only, pass --allow-unsigned.",
            path.display()
        );
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading public key {}", path.display()))?;
    let fields = parse_tagged(&text, PUBKEY_TAG, 2)
        .with_context(|| format!("parsing {}", path.display()))?;
    let raw = b64()
        .decode(&fields[0])
        .context("public key is not valid base64")?;
    let bytes: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&bytes).context("public key is not a valid Ed25519 point")?;

    let stated = &fields[1];
    let actual = key_id(&vk);
    if stated != &actual {
        bail!("public key file claims id {stated} but the key hashes to {actual}");
    }
    Ok((vk, actual))
}

/// The signing key comes from an environment variable rather than a path so
/// that CI can pass it as a secret without it ever touching the filesystem.
fn signing_key_from_env(var: &str) -> Result<Option<SigningKey>> {
    let raw = match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    // Accept either the bare base64 seed or the full tagged secret-key file.
    let b64_seed = if raw.contains(SECKEY_TAG) {
        parse_tagged(&raw, SECKEY_TAG, 1)?[0].clone()
    } else {
        raw.trim().to_string()
    };
    let bytes: [u8; 32] = b64()
        .decode(b64_seed)
        .with_context(|| format!("{var} is not valid base64"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("{var} must decode to a 32-byte seed"))?;
    Ok(Some(SigningKey::from_bytes(&bytes)))
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Generate a key pair. The maintainer runs this once, locally: the public half
/// is committed as the trust root, the private half becomes a CI secret. It is
/// deliberately not something CI can do for itself.
///
/// The two halves go to different places on purpose. The public key belongs in
/// the repository; the private key belongs outside every working tree, and this
/// refuses to write it into one.
pub fn keygen(pubkey: Option<&str>, secret: Option<&str>) -> Result<()> {
    use rand::RngCore;

    let pub_path = PathBuf::from(pubkey.unwrap_or(DEFAULT_PUBKEY_PATH));
    let sec_path = match secret {
        Some(s) => PathBuf::from(s),
        None => default_secret_path()?,
    };

    // A signing key inside a checkout is one `git add -f` or one stray archive
    // away from being published, so refuse rather than rely on .gitignore.
    if let Some(root) = repo_root() {
        let root = root.canonicalize().unwrap_or(root);
        if absolutise(&sec_path).starts_with(&root) {
            bail!(
                "refusing to write the private key inside the working tree at {}.\n\
                 {} is in the repository, where a signing key does not belong.\n\
                 Leave --secret unset to use {}, or pass a path outside the repo.",
                root.display(),
                sec_path.display(),
                default_secret_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "your config directory".into())
            );
        }
    }

    for p in [&pub_path, &sec_path] {
        if p.exists() {
            bail!(
                "{} already exists; refusing to overwrite a signing key",
                p.display()
            );
        }
    }

    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let sk = SigningKey::from_bytes(&seed);
    let vk = sk.verifying_key();
    let id = key_id(&vk);

    for (path, private) in [(&pub_path, false), (&sec_path, true)] {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
                // The directory holding a private key should not be readable by
                // other accounts either.
                #[cfg(unix)]
                if private {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
                }
            }
        }
    }

    fs::write(
        &pub_path,
        format!(
            "# SQL Anywhere extension signing key (public). Safe to commit.\n\
             # Key id {id}\n\
             {PUBKEY_TAG} {} {id}\n",
            b64().encode(vk.as_bytes())
        ),
    )
    .with_context(|| format!("writing {}", pub_path.display()))?;

    fs::write(
        &sec_path,
        format!(
            "# SQL Anywhere extension signing key (PRIVATE). Never commit this.\n\
             # Key id {id}\n\
             {SECKEY_TAG} {}\n",
            b64().encode(seed)
        ),
    )
    .with_context(|| format!("writing {}", sec_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&sec_path, fs::Permissions::from_mode(0o600));
    }

    println!("key id:      {id}");
    println!("public key:  {}", pub_path.display());
    println!("secret key:  {}  (mode 600)", sec_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Commit {} as the trust root.", pub_path.display());
    println!("     CI reads it from a fresh checkout, so an uncommitted key has no effect.");
    println!("  2. Store the secret as the CI secret EXTENSION_SIGNING_KEY:");
    println!(
        "       gh secret set EXTENSION_SIGNING_KEY < {}",
        sec_path.display()
    );
    println!("  3. Keep an offline copy. Losing the secret means rotating the key.");
    Ok(())
}

/// Describe a directory of release artifacts and, when a key is available, sign
/// the description.
pub fn sign(dir: &str, release: &str) -> Result<()> {
    let dir = Path::new(dir);
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }

    let api_version = api_version_from_header(&find_header()?)?;

    // BTreeMap keeps the manifest ordered by file name, so the same inputs
    // always produce the same bytes.
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for e in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = e?.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !path.is_file() || matches!(name.as_str(), MANIFEST_NAME | SIG_NAME | SUMS_NAME) {
            continue;
        }
        found.insert(name, path);
    }
    if found.is_empty() {
        bail!("no artifacts found in {}", dir.display());
    }

    let mut extensions = Vec::new();
    let mut sums = String::new();
    for (name, path) in &found {
        let (sha256, size) = sha256_file(path)?;
        let (ext_name, target) = split_artifact_name(name, release);
        sums.push_str(&format!("{sha256}  {name}\n"));
        extensions.push(Entry {
            name: ext_name,
            target,
            file: name.clone(),
            size,
            sha256,
            sqlanywhere_api_version: api_version,
        });
    }

    let signing_key = signing_key_from_env("EXTENSION_SIGNING_KEY")?;
    let manifest = Manifest {
        schema: 1,
        release: release.to_string(),
        signing_key: signing_key.as_ref().map(|k| key_id(&k.verifying_key())),
        extensions,
    };

    // Sign the bytes we write, not a re-serialisation of the structure.
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(dir.join(MANIFEST_NAME), &bytes)?;
    fs::write(dir.join(SUMS_NAME), sums)?;

    for e in &manifest.extensions {
        println!("  {:<12} {:<28} {}", e.name, e.target, &e.sha256[..16]);
    }
    println!(
        "{} artifact(s), built against extension interface version {api_version}",
        manifest.extensions.len()
    );

    match signing_key {
        Some(sk) => {
            let sig = sk.sign(&bytes);
            let id = key_id(&sk.verifying_key());
            fs::write(
                dir.join(SIG_NAME),
                format!("{SIG_TAG} {id} {}\n", b64().encode(sig.to_bytes())),
            )?;
            println!("signed {MANIFEST_NAME} with key {id} -> {SIG_NAME}");
        }
        None => {
            println!();
            println!("WARNING: EXTENSION_SIGNING_KEY is not set, so this release is UNSIGNED.");
            println!("         {SUMS_NAME} still gives integrity, but not authenticity.");
            println!("         Run `cargo xtask extension-keygen` and set the CI secret.");
        }
    }
    Ok(())
}

/// Verify a release directory: signature over the manifest, then every digest.
pub fn verify(dir: &str, pubkey: Option<&str>, allow_unsigned: bool) -> Result<()> {
    let dir = Path::new(dir);
    let manifest_path = dir.join(MANIFEST_NAME);
    let bytes =
        fs::read(&manifest_path).with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {MANIFEST_NAME}"))?;

    if manifest.schema != 1 {
        bail!(
            "{MANIFEST_NAME} uses schema {} but this tool understands 1",
            manifest.schema
        );
    }

    let sig_path = dir.join(SIG_NAME);
    if sig_path.is_file() {
        let key_path = PathBuf::from(pubkey.unwrap_or(DEFAULT_PUBKEY_PATH));
        let (vk, id) = read_pubkey(&key_path)?;

        let text = fs::read_to_string(&sig_path)?;
        let fields = parse_tagged(&text, SIG_TAG, 2)
            .with_context(|| format!("parsing {}", sig_path.display()))?;
        if fields[0] != id {
            bail!(
                "{SIG_NAME} was made by key {} but the trusted key is {id}",
                fields[0]
            );
        }
        let raw = b64()
            .decode(&fields[1])
            .context("signature is not base64")?;
        let sig_bytes: [u8; 64] = raw
            .try_into()
            .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
        vk.verify(&bytes, &Signature::from_bytes(&sig_bytes))
            .context("signature does not match the manifest; do not load these artifacts")?;
        println!("signature OK (key {id})");
    } else if allow_unsigned {
        println!("WARNING: no {SIG_NAME}; checking integrity only, not authenticity");
    } else {
        bail!(
            "{} is missing; pass --allow-unsigned to check digests only",
            sig_path.display()
        );
    }

    let host = api_version_from_header(&find_header()?).ok();
    let mut checked = 0usize;
    let mut missing = Vec::new();
    for e in &manifest.extensions {
        let path = dir.join(&e.file);
        if !path.is_file() {
            missing.push(e.file.clone());
            continue;
        }
        let (sha256, size) = sha256_file(&path)?;
        if sha256 != e.sha256 {
            bail!(
                "{} has digest {sha256} but the manifest says {}",
                e.file,
                e.sha256
            );
        }
        if size != e.size {
            bail!(
                "{} is {size} bytes but the manifest says {}",
                e.file,
                e.size
            );
        }
        if let Some(host) = host {
            if e.sqlanywhere_api_version > host {
                bail!(
                    "{} needs extension interface version {} but this tree implements {host}",
                    e.file,
                    e.sqlanywhere_api_version
                );
            }
        }
        checked += 1;
        println!("  ok  {}", e.file);
    }

    for m in &missing {
        println!("  --  {m} (listed in the manifest, not present here)");
    }
    if checked == 0 {
        bail!(
            "none of the manifest's artifacts are present in {}",
            dir.display()
        );
    }
    println!("{checked} artifact(s) verified");
    Ok(())
}
