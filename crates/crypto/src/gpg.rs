use anyhow::{Result, anyhow};
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub enum DecryptError {
    NotForMe { stderr: String },
    InvalidMessage { stderr: String },
    GpgFailed { stderr: String },
    Io(String),
}

impl fmt::Display for DecryptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecryptError::NotForMe { .. } => write!(f, "not for me"),
            DecryptError::InvalidMessage { .. } => write!(f, "invalid pgp message"),
            DecryptError::GpgFailed { .. } => write!(f, "gpg failed"),
            DecryptError::Io(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for DecryptError {}

fn classify_decrypt_failure(stderr: &str) -> DecryptError {
    let s = stderr.to_lowercase();

    if s.contains("no secret key")
        || s.contains("decryption failed: no secret key")
        || s.contains("secret key not available")
    {
        return DecryptError::NotForMe {
            stderr: stderr.to_string(),
        };
    }

    if s.contains("no valid openpgp data found")
        || s.contains("invalid armor header")
        || s.contains("crc error")
        || s.contains("unexpected end of file")
        || s.contains("bad armor")
        || s.contains("invalid packet")
    {
        return DecryptError::InvalidMessage {
            stderr: stderr.to_string(),
        };
    }

    DecryptError::GpgFailed {
        stderr: stderr.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct PublicKey {
    pub fpr: String,
    pub uid: Option<String>,
}

/// List public keys
pub fn list_public_keys() -> Result<Vec<PublicKey>> {
    let out = Command::new("gpg")
        .args(["--batch", "--with-colons", "--list-keys"])
        .output()
        .map_err(|e| anyhow!("Failed to run gpg: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("gpg list-keys failed: {err}"));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Each key has one or more `uid:` lines and one or more `fpr:` lines
    // first fpr after a pub entry corresponds to that key's primary fingerprint
    // Capture the first uid seen for a key and the next fpr.
    let mut res = Vec::new();
    let mut pending_uid: Option<String> = None;
    let mut saw_pub = false;

    for line in stdout.lines() {
        if line.starts_with("pub:") {
            saw_pub = true;
            pending_uid = None;
            continue;
        }
        if !saw_pub {
            continue;
        }

        if line.starts_with("uid:") && pending_uid.is_none() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() > 9 {
                let uid = parts[9].trim();
                if !uid.is_empty() {
                    pending_uid = Some(uid.to_string());
                }
            }
        }

        if line.starts_with("fpr:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() > 9 {
                let fpr = parts[9].trim();
                if !fpr.is_empty() {
                    res.push(PublicKey {
                        fpr: fpr.to_string(),
                        uid: pending_uid.take(),
                    });

                    // wait for next pub:
                    saw_pub = false;
                }
            }
        }
    }

    Ok(res)
}

/// Encrypt plaintext to a recipient (fingerprint or uid)
pub fn encrypt_to_recipient(recipient: &str, plaintext: &str) -> Result<String> {
    // TODO: --trust-model for now
    // avoids "untrusted key" prompt
    let mut child = Command::new("gpg")
        .args([
            "--batch",
            "--yes",
            "--armor",
            "--encrypt",
            "--trust-model",
            "always",
            "-r",
            recipient,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn gpg: {e}"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("Failed to open gpg stdin"))?;
        stdin
            .write_all(plaintext.as_bytes())
            .map_err(|e| anyhow!("Failed writing to gpg stdin: {e}"))?;
    }

    let out = child
        .wait_with_output()
        .map_err(|e| anyhow!("Failed to read gpg output: {e}"))?;

    if out.status.success() {
        let armored =
            String::from_utf8(out.stdout).map_err(|e| anyhow!("gpg stdout not utf8: {e}"))?;
        Ok(armored)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        Err(anyhow!("gpg encrypt failed: {err}"))
    }
}

pub fn decrypt(armored: &str) -> std::result::Result<String, DecryptError> {
    let mut child = Command::new("gpg")
        .args(["--batch", "--decrypt"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DecryptError::Io(format!("Failed to spawn gpg: {e}")))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| DecryptError::Io("Failed to open gpg stdin".into()))?;
        stdin
            .write_all(armored.as_bytes())
            .map_err(|e| DecryptError::Io(format!("Failed writing to gpg stdin: {e}")))?;
    }

    let out = child
        .wait_with_output()
        .map_err(|e| DecryptError::Io(format!("Failed to read gpg output: {e}")))?;

    if out.status.success() {
        let plaintext = String::from_utf8(out.stdout)
            .map_err(|e| DecryptError::Io(format!("gpg stdout not utf8: {e}")))?;
        Ok(plaintext)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        Err(classify_decrypt_failure(&err))
    }
}

/// Check if `gpg` can be executed
pub fn available() -> Result<bool> {
    let out = Command::new("gpg").arg("--version").output();
    match out {
        Ok(o) => Ok(o.status.success()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow!("Failed to run gpg: {e}")),
    }
}

/// Get gpg version
pub fn version_line() -> Result<String> {
    let out = Command::new("gpg")
        .arg("--version")
        .output()
        .map_err(|e| anyhow!("Failed to run gpg: {e}"))?;

    if !out.status.success() {
        return Err(anyhow!("gpg --version failed"));
    }

    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.lines().next().unwrap_or("").to_string())
}

pub fn list_secret_fingerprints() -> Result<Vec<String>> {
    let out = Command::new("gpg")
        .args(["--batch", "--with-colons", "--list-secret-keys"])
        .output()
        .map_err(|e| anyhow!("Failed to run gpg: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("gpg list-secret-keys failed: {err}"));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_colons_fprs(&stdout))
}

fn parse_colons_fprs(colons: &str) -> Vec<String> {
    let mut res = Vec::new();
    for line in colons.lines() {
        if line.starts_with("fpr:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() > 9 {
                let fpr = parts[9].trim();
                if !fpr.is_empty() {
                    res.push(fpr.to_string())
                }
            }
        }
    }

    res.sort();
    res.dedup();
    res
}
