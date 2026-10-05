//! The `.bmsnap` container — one self-describing file that carries a
//! Buildmesh state snapshot or export (issue #1537).
//!
//! ## Why a container at all
//!
//! The durable state is more than one file (`buildmesh.db`, `preferences.json`),
//! and a naive copy of the app-data folder is the leak the issue calls out: it
//! sweeps in `tls/ca.key.der` and the cleartext `remote_access_token` without
//! the user ever seeing them. A container forces three properties that a folder
//! copy cannot give:
//!
//! 1. **A versioned format** the restore path can reject *before* touching
//!    live data (a future build's bundle must not be silently half-applied).
//! 2. **Per-section checksums** so a truncated or edited file is detected
//!    before restore, not halfway through it.
//! 3. **An explicit section list** — including *absent* sections, which is how
//!    "this export contains no TLS keys" becomes a checkable fact rather than
//!    an assumption about what the exporter happened to include.
//!
//! ## Layout
//!
//! ```text
//! 0            8                        12 + header_len
//! +------------+-------------------------+---------------------------+
//! | "BMSNAP\x1a"| u32 LE header_len      | JSON header               |
//! +------------+-------------------------+---------------------------+
//!                                     section payloads, back to back
//! ```
//!
//! Section payloads live in one trailing blob at known `(offset, length)`
//! pairs rather than being interleaved with the header, because the header
//! cannot know its own serialized length until every section's offset is
//! known — and an offset cannot be known before the header length is. The
//! writer resolves the cycle the simple way: payloads go to a temp file
//! first, then the final file is header + a streaming copy.
//!
//! `VACUUM INTO` (not a file copy) produces the `state.db` payload, so a
//! snapshot taken while the app is running is WAL-consistent rather than a
//! torn copy of a database mid-checkpoint.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::http::tls::protect_private_directory;
use crate::http::tls::protect_private_file;

/// File magic. The trailing `\x1a` (SUB) and NUL stop a naive `type`/text peek
/// from rendering the file as readable content on systems that sniff by type.
pub(crate) const MAGIC: [u8; 8] = *b"BMSNAP\x1a\x00";

/// Container format version. Bump on any incompatible layout or field change;
/// [`BundleReader::open`] refuses a bundle whose version is newer, so an
/// older build cannot half-interpret a newer bundle (issue #1537: "corrupt /
/// unsupported restore is rejected without altering current data").
pub(crate) const FORMAT_VERSION: u32 = 1;

/// Ceiling on the serialized header. The header is a small JSON object
/// naming a handful of sections; anything larger means the file is not one of
/// ours (or is corrupt), and allocating it up front would be the DoS.
const MAX_HEADER_BYTES: u32 = 1 << 20;

/// Ceiling on a single section, mirroring the "this is a local state file,
/// not an archive" contract. Guards `read_section` against a hostile or
/// corrupt length field.
const MAX_SECTION_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Streaming copy buffer.
const COPY_BUF: usize = 64 * 1024;

/// One payload inside the container.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BundleSection {
    /// Stable name callers look sections up by (`state.db`,
    /// `preferences.json`). Never a user-controlled string.
    pub name: String,
    /// Byte offset of the payload within the trailing blob.
    pub offset: u64,
    pub length: u64,
    /// Lowercase hex SHA-256 of the payload — the integrity gate.
    pub sha256: String,
}

/// The container's self-description.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BundleHeader {
    pub format_version: u32,
    /// `snapshot` (automatic, full fidelity) or `export` (user-initiated).
    pub kind: String,
    /// RFC 3339 UTC. Injected rather than read from the clock so tests pin it.
    pub created_at: String,
    pub app_version: String,
    /// The `schema_version` the bundled database was at when it was captured.
    /// A restore from an older bundle is legal — the migration runner evolves
    /// it forward — so this is reported, never used to refuse.
    pub schema_version: u32,
    /// Whether credentials were stripped. A restore surfaces this so the UI
    /// can warn that remote-access credentials will be re-minted.
    pub redacted: bool,
    pub sections: Vec<BundleSection>,
}

impl BundleHeader {
    pub(crate) fn section(&self, name: &str) -> Option<&BundleSection> {
        self.sections.iter().find(|s| s.name == name)
    }
}

/// Header fields fixed before any payload is known.
pub(crate) struct BundlePrologue {
    pub kind: String,
    pub created_at: String,
    pub app_version: String,
    pub schema_version: u32,
    pub redacted: bool,
}

/// Write a container.
///
/// Sections are staged in a temp file so the header can be serialized with
/// final offsets; see the module docs for why the layout is shaped that way.
pub(crate) fn write_bundle(
    dest: &Path,
    prologue: BundlePrologue,
    sections: &[(&str, Vec<u8>)],
) -> io::Result<BundleHeader> {
    let mut payload = tempfile::NamedTempFile::new()?;
    let mut described = Vec::with_capacity(sections.len());
    let mut offset: u64 = 0;
    for (name, bytes) in sections {
        payload.write_all(bytes)?;
        described.push(BundleSection {
            name: (*name).to_string(),
            offset,
            length: bytes.len() as u64,
            sha256: hex_digest(bytes),
        });
        offset += bytes.len() as u64;
    }
    payload.flush()?;

    let header = BundleHeader {
        format_version: FORMAT_VERSION,
        kind: prologue.kind,
        created_at: prologue.created_at,
        app_version: prologue.app_version,
        schema_version: prologue.schema_version,
        redacted: prologue.redacted,
        sections: described,
    };
    let header_bytes = serde_json::to_vec(&header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if header_bytes.len() as u64 > MAX_HEADER_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle header exceeds the maximum size",
        ));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
        protect_private_directory(parent)?;
    }
    // Same reason as a temp-file rename: a crash mid-write must not leave a
    // half container that `verify_bundle` would later have to reason about.
    // Write beside the destination so the rename stays on one volume.
    let staged = dest.with_extension("bmsnap-partial");
    {
        let mut out = BufWriter::new(File::create(&staged)?);
        out.write_all(&MAGIC)?;
        out.write_all(&(header_bytes.len() as u32).to_le_bytes())?;
        out.write_all(&header_bytes)?;
        let mut reader = payload.reopen()?;
        let mut buf = vec![0u8; COPY_BUF];
        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            out.write_all(&buf[..read])?;
        }
        out.flush()?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    protect_private_file(&staged)?;
    std::fs::rename(&staged, dest)?;
    Ok(header)
}

/// Read-side handle over a container.
pub(crate) struct BundleReader {
    file: File,
    header: BundleHeader,
    /// Offset in the file at which the payload blob starts.
    data_start: u64,
}

/// Why a container could not be used. Distinct from [`io::Error`] so the
/// restore path can tell "this is not a Buildmesh bundle" (reject, no state
/// touched) from "the disk is broken" (surface a real error).
#[derive(Debug)]
pub(crate) enum BundleError {
    /// The file is not a container we can read — wrong magic, truncated, a
    /// header that does not parse, a section pointing outside the file, or a
    /// checksum that does not match.
    Invalid(String),
    Io(io::Error),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleError::Invalid(message) => write!(f, "invalid Buildmesh state bundle: {message}"),
            BundleError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for BundleError {
    fn from(e: io::Error) -> Self {
        BundleError::Io(e)
    }
}

impl BundleReader {
    /// Open a container and validate everything that can be validated without
    /// reading the payloads: magic, header length, header JSON, format
    /// version, and that every section lies inside the file.
    ///
    /// This is deliberately the *only* gate a restore runs before it decides
    /// to touch live data, so a structurally bad bundle costs nothing.
    pub(crate) fn open(path: &Path) -> Result<Self, BundleError> {
        let mut file = File::open(path)?;
        let total = file
            .metadata()
            .map_err(|e| BundleError::Invalid(format!("stat failed: {e}")))?
            .len();

        let mut magic = [0u8; 8];
        read_exact_at(&mut file, 0, &mut magic)
            .map_err(|e| BundleError::Invalid(format!("file is too small to be a bundle: {e}")))?;
        if magic != MAGIC {
            return Err(BundleError::Invalid(
                "missing the BMSNAP file signature".to_string(),
            ));
        }

        let mut len_bytes = [0u8; 4];
        read_exact_at(&mut file, 8, &mut len_bytes)
            .map_err(|e| BundleError::Invalid(format!("truncated header length: {e}")))?;
        let header_len = u32::from_le_bytes(len_bytes);
        if header_len == 0 || header_len > MAX_HEADER_BYTES {
            return Err(BundleError::Invalid(format!(
                "implausible header length ({header_len} bytes)"
            )));
        }
        let data_start = 12u64 + header_len as u64;
        if data_start > total {
            return Err(BundleError::Invalid(
                "header extends past the end of the file".to_string(),
            ));
        }

        let mut header_bytes = vec![0u8; header_len as usize];
        read_exact_at(&mut file, 12, &mut header_bytes)
            .map_err(|e| BundleError::Invalid(format!("truncated header: {e}")))?;
        let header: BundleHeader = serde_json::from_slice(&header_bytes)
            .map_err(|e| BundleError::Invalid(format!("header does not parse: {e}")))?;

        if header.format_version > FORMAT_VERSION {
            return Err(BundleError::Invalid(format!(
                "bundle format version {} is newer than this build supports ({FORMAT_VERSION})",
                header.format_version
            )));
        }

        let payload_len = total - data_start;
        for section in &header.sections {
            if section.length > MAX_SECTION_BYTES {
                return Err(BundleError::Invalid(format!(
                    "section `{}` declares an implausible length",
                    section.name
                )));
            }
            let end = section
                .offset
                .checked_add(section.length)
                .ok_or_else(|| BundleError::Invalid("section range overflows".to_string()))?;
            if end > payload_len {
                return Err(BundleError::Invalid(format!(
                    "section `{}` extends past the end of the file",
                    section.name
                )));
            }
        }

        Ok(Self {
            file,
            header,
            data_start,
        })
    }

    pub(crate) fn header(&self) -> &BundleHeader {
        &self.header
    }

    /// Recompute every section checksum. The full integrity gate, run by
    /// `verify_bundle` before a restore is staged.
    pub(crate) fn verify(&mut self) -> Result<(), BundleError> {
        for section in self.header.sections.clone() {
            let mut hasher = Sha256::new();
            let mut remaining = section.length;
            let mut position = self.data_start + section.offset;
            let mut buf = vec![0u8; COPY_BUF];
            while remaining > 0 {
                let want = remaining.min(COPY_BUF as u64) as usize;
                let read = read_at(&mut self.file, position, &mut buf[..want])?;
                hasher.update(&buf[..read]);
                position += read as u64;
                remaining -= read as u64;
            }
            let actual = hex::encode(hasher.finalize());
            if actual != section.sha256 {
                return Err(BundleError::Invalid(format!(
                    "section `{}` failed its checksum — the file is corrupt or was edited",
                    section.name
                )));
            }
        }
        Ok(())
    }

    /// Read one section fully into memory.
    pub(crate) fn read_section(&mut self, name: &str) -> Result<Vec<u8>, BundleError> {
        let section = self
            .header
            .section(name)
            .ok_or_else(|| {
                BundleError::Invalid(format!("bundle has no `{name}` section"))
            })?
            .clone();
        if section.length > MAX_SECTION_BYTES {
            return Err(BundleError::Invalid(format!(
                "section `{name}` is too large to read into memory"
            )));
        }
        let mut bytes = vec![0u8; section.length as usize];
        read_at(&mut self.file, self.data_start + section.offset, &mut bytes)?;
        Ok(bytes)
    }

    /// Stream one section to `dest` without buffering it, so a large database
    /// never lands in memory twice.
    pub(crate) fn extract_section_to(&mut self, name: &str, dest: &Path) -> Result<u64, BundleError> {
        let section = self
            .header
            .section(name)
            .ok_or_else(|| {
                BundleError::Invalid(format!("bundle has no `{name}` section"))
            })?
            .clone();
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = BufWriter::new(File::create(dest)?);
        let mut position = self.data_start + section.offset;
        let mut remaining = section.length;
        let mut buf = vec![0u8; COPY_BUF];
        while remaining > 0 {
            let want = remaining.min(COPY_BUF as u64) as usize;
            let read = read_at(&mut self.file, position, &mut buf[..want])?;
            out.write_all(&buf[..read])?;
            position += read as u64;
            remaining -= read as u64;
        }
        out.flush()?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(section.length)
    }
}

/// Open, parse, and fully checksum a container. The single gate the restore
/// path runs before it writes anything.
pub(crate) fn verify_bundle(path: &Path) -> Result<BundleHeader, BundleError> {
    let mut reader = BundleReader::open(path)?;
    reader.verify()?;
    Ok(reader.header().clone())
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// `read_exact` at an absolute offset without disturbing a shared cursor
/// beyond what the caller expects — each read seeks, so a reader can walk
/// sections in any order.
fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(&mut *file);
    let mut filled = 0;
    while filled < buf.len() {
        let read = reader.read(&mut buf[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

fn read_exact_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    let read = read_at(file, offset, buf)?;
    if read != buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("expected {} bytes at offset {offset}, got {read}", buf.len()),
        ));
    }
    Ok(())
}

/// Where a bundle's staged restore payload waits for the next launch.
pub(crate) const PENDING_DIR: &str = "pending-restore";
/// The marker naming which sections the pending restore will apply.
pub(crate) const PENDING_MARKER: &str = "restore.json";
/// Sections a restore knows how to apply, in apply order. Keeping this an
/// explicit list is what stops a future exporter from smuggling an unknown
/// section into the live state directory.
pub(crate) const SECTION_DB: &str = "state.db";
pub(crate) const SECTION_PREFS: &str = "preferences.json";

/// The staged-restore marker. Written only after both sections have been
/// extracted and flushed, so its presence means "a complete, verified
/// payload is waiting" — a crash mid-stage leaves no marker and therefore
/// no half-applied restore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PendingRestore {
    pub source: String,
    pub created_at: String,
    pub redacted: bool,
    pub schema_version: u32,
    /// Basenames present in the pending directory.
    pub files: Vec<String>,
}

pub(crate) fn pending_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(PENDING_DIR)
}
