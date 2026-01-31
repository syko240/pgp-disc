
/// Extract block if pgp
pub fn extract_pgp_message_block(input: &str) -> Option<String> {
    const BEGIN: &str = "-----BEGIN PGP MESSAGE-----";
    const END: &str = "-----END PGP MESSAGE-----";

    let start = input.find(BEGIN)?;
    let after_start = &input[start..];
    let end_rel = after_start.find(END)?;
    let end_abs = start + end_rel + END.len();

    // extract pgp block
    let block = &input[start..end_abs];

    Some(block.trim().to_string())
}

/// Extract stable id from block
pub fn pgp_block_id(block: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(block.as_bytes());
    let digest = hasher.finalize();
    // return id
    hex::encode(&digest[..8])
}

pub fn detect_pgp(input: &str) -> Option<(String, String)> {
    let block = extract_pgp_message_block(input)?;
    let id = pgp_block_id(&block);
    Some((id, block))
}
