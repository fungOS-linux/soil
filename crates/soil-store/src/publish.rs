//! Publishing operations: the operator's delegation and the publisher's
//! signatures.
//!
//! A store operator signs **publisher keys**, not catalog revisions: a key is
//! delegated once and withdrawn by an explicit record, so a publisher can append
//! an entry without a store round-trip. This module is the code that produces
//! those records, so nobody has to hand-assemble a payload and pipe it through
//! `gpg`.
//!
//! The fingerprints and the gpg invocations live here for the same reason the
//! payload definitions live in [`crate::index`]: one spelling of the bytes that
//! get signed, on the side that signs and the side that verifies.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::index::{Delegation, Index, Revocation};
use crate::{Error, Result};

/// What `soil-store delegate` was asked for.
pub struct DelegationRequest<'a> {
    pub publisher: &'a str,
    /// Fingerprint or user id of the operator's secret key in `home`.
    pub operator_key: &'a str,
    pub home: Option<&'a Path>,
    pub scope: &'a str,
    /// `0` means the delegation never expires.
    pub days: u64,
    /// An armored public key file for the publisher.
    pub public_key: Option<&'a Path>,
    /// Or a fingerprint of a key already in the operator's keyring.
    pub fingerprint: Option<&'a str>,
}

/// What `soil-store sign` was asked for.
pub struct SignRequest<'a> {
    pub publisher: &'a str,
    /// The publisher's key: it signs the entries.
    pub key: &'a str,
    pub home: Option<&'a Path>,
    /// The operator's key, if the checkpoint should be countersigned.
    pub countersign: Option<&'a str>,
}

/// Build and sign a delegation of one publisher key to one principal.
pub fn delegate(request: DelegationRequest<'_>) -> Result<Delegation> {
    if request.public_key.is_some() == request.fingerprint.is_some() {
        return Err(Error::Invalid(
            "delegate needs exactly one of --public-key FILE or --fingerprint KEY".into(),
        ));
    }
    if !matches!(request.scope, "spore" | "*") {
        return Err(Error::Invalid(format!(
            "scope {:?} is not one of spore, *",
            request.scope
        )));
    }
    let (fingerprint, public_key) = match (request.public_key, request.fingerprint) {
        (Some(path), None) => {
            let armored = std::fs::read_to_string(path)
                .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
            (fingerprint_of(&armored)?, armored)
        }
        (None, Some(fingerprint)) => (
            fingerprint.to_ascii_uppercase(),
            export_public_key(request.home, fingerprint)?,
        ),
        _ => unreachable!("checked above"),
    };

    let issued_at = now_unix_s();
    let mut delegation = Delegation {
        publisher: request.publisher.to_owned(),
        fingerprint,
        public_key,
        scope: request.scope.to_owned(),
        issued_at,
        expires_at: match request.days {
            0 => 0,
            days => issued_at + days * 86_400,
        },
        signature: String::new(),
    };
    delegation.signature = soil_spore::sign_detached(
        request.home,
        request.operator_key,
        delegation.payload().as_bytes(),
    )?;
    Ok(delegation)
}

/// Sign every entry this publisher owns and reseal the checkpoint.
///
/// Entries already signed by someone else are left alone; entries with no
/// publisher are adopted. `seq` is only assigned where it is still zero, so
/// appending to an existing log never renumbers history. Returns how many
/// entries were signed.
pub fn sign(index: &mut Index, request: SignRequest<'_>) -> Result<usize> {
    let fingerprint = request.key.to_ascii_uppercase();
    let mut signed = 0;
    let mut previous = 0;
    for entry in &mut index.entries {
        // Sequence numbers belong to the log, not to whoever signs it.
        if entry.seq == 0 {
            entry.seq = previous + 1;
        }
        previous = entry.seq;
        match entry.publisher.as_deref() {
            Some(publisher) if publisher != request.publisher => continue,
            Some(_) | None => {}
        }
        entry.publisher = Some(request.publisher.to_owned());
        entry.publisher_key = Some(fingerprint.clone());
        entry.signature =
            soil_spore::sign_detached(request.home, &fingerprint, entry.payload().as_bytes())?;
        signed += 1;
    }
    if signed == 0 {
        return Err(Error::Invalid(format!(
            "no entries belong to {}",
            request.publisher
        )));
    }
    reseal(index);
    if let Some(operator) = request.countersign {
        index.checkpoint.signature = soil_spore::sign_detached(
            request.home,
            operator,
            index.checkpoint.payload(&index.source).as_bytes(),
        )?;
    }
    Ok(signed)
}

/// Withdraw one delegated key from a publisher.
pub fn revoke(
    index: &mut Index,
    publisher: &str,
    key: &str,
    reason: &str,
    operator_key: &str,
    home: Option<&Path>,
) -> Result<Revocation> {
    if reason.trim().is_empty() {
        return Err(Error::Invalid("--reason must say why".into()));
    }
    let fingerprint = key.to_ascii_uppercase();
    if index.revocations.iter().any(|revocation| {
        revocation.publisher == publisher
            && revocation.fingerprint.eq_ignore_ascii_case(&fingerprint)
    }) {
        return Err(Error::Invalid(format!(
            "{publisher} key {fingerprint} is already revoked"
        )));
    }
    let mut revocation = Revocation {
        publisher: publisher.to_owned(),
        fingerprint,
        revoked_at: now_unix_s(),
        reason: reason.to_owned(),
        signature: String::new(),
    };
    revocation.signature =
        soil_spore::sign_detached(home, operator_key, revocation.payload().as_bytes())?;
    index.revocations.push(revocation.clone());
    Ok(revocation)
}

/// Merge delegations into an index, replacing any earlier record for the same
/// publisher and key so a rotated or re-issued delegation wins.
pub fn merge_delegations(index: &mut Index, delegations: Vec<Delegation>) -> usize {
    let mut merged = 0;
    for delegation in delegations {
        let key = (
            delegation.publisher.clone(),
            delegation.fingerprint.to_ascii_uppercase(),
        );
        index.delegations.retain(|existing| {
            (
                existing.publisher.clone(),
                existing.fingerprint.to_ascii_uppercase(),
            ) != key
        });
        index.delegations.push(delegation);
        merged += 1;
    }
    merged
}

/// Recompute the checkpoint over the current entries.
fn reseal(index: &mut Index) {
    index.checkpoint.seq = index
        .entries
        .iter()
        .map(|entry| entry.seq)
        .max()
        .unwrap_or(0);
    index.checkpoint.root = Index::root(&index.entries);
}

/// The fingerprint of the first key in an armored block, as GnuPG reads it.
fn fingerprint_of(public_key: &str) -> Result<String> {
    let (status, stdout) = gpg(
        None,
        &["--with-colons", "--import-options", "show-only", "--import"],
        Some(public_key.as_bytes()),
    )?;
    if !status {
        return Err(Error::Invalid("gpg could not read the public key".into()));
    }
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("fpr:"))
        .and_then(|rest| rest.split(':').nth(8))
        .filter(|fingerprint| !fingerprint.is_empty())
        .map(|fingerprint| fingerprint.to_ascii_uppercase())
        .ok_or_else(|| Error::Invalid("public key has no fingerprint".into()))
}

/// Export an armored public key by fingerprint from `home`.
pub fn export_public_key(home: Option<&Path>, fingerprint: &str) -> Result<String> {
    let (status, stdout) = gpg(home, &["--armor", "--export", fingerprint], None)?;
    if !status || stdout.trim().is_empty() {
        return Err(Error::Invalid(format!(
            "no public key {fingerprint} in the keyring"
        )));
    }
    Ok(stdout)
}

/// Run gpg and return whether it succeeded plus its stdout.
fn gpg(home: Option<&Path>, args: &[&str], stdin: Option<&[u8]>) -> Result<(bool, String)> {
    let mut command = Command::new("gpg");
    if let Some(home) = home {
        command.arg("--homedir").arg(home);
    }
    command.args(["--batch", "--no-tty"]).args(args);
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| Error::Tool(format!("gpg: {error}")))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input)
            .map_err(|error| Error::Tool(format!("gpg: {error}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| Error::Tool(format!("gpg: {error}")))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
