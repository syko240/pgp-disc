use anyhow::{Result, anyhow};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn decrypt(armored: &str) -> Result<String> {
    let mut child = Command::new("gpg")
        .args(["--batch", "--decrypt"])
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
            .write_all(armored.as_bytes())
            .map_err(|e| anyhow!("Failed writing to gpg stdin: {e}"))?;
    }

    let out = child
        .wait_with_output()
        .map_err(|e| anyhow!("Failed to read gpg output: {e}"))?;

    if out.status.success() {
        let plaintext =
            String::from_utf8(out.stdout).map_err(|e| anyhow!("gpg stdout not utf8: {e}"))?;
        Ok(plaintext)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        Err(anyhow!("gpg decrypt failed: {err}"))
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
