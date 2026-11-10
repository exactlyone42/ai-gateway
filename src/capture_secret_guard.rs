use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, thiserror::Error)]
#[error("capture contains a protected credential")]
pub(crate) struct CaptureSecretError;

pub(crate) struct CaptureSecretGuard {
    secrets: Vec<Zeroizing<Vec<u8>>>,
    tails: [Zeroizing<Vec<u8>>; 4],
    max_tail: usize,
    failed: bool,
}

impl CaptureSecretGuard {
    pub(crate) fn new(secrets: &[&[u8]]) -> Result<Self, CaptureSecretError> {
        if secrets.len() > 32
            || secrets
                .iter()
                .any(|secret| secret.is_empty() || secret.len() > 4096)
        {
            return Err(CaptureSecretError);
        }
        Ok(Self {
            secrets: secrets
                .iter()
                .map(|secret| Zeroizing::new(secret.to_vec()))
                .collect(),
            tails: std::array::from_fn(|_| Zeroizing::new(Vec::new())),
            max_tail: secrets
                .iter()
                .map(|secret| secret.len() - 1)
                .max()
                .unwrap_or(0),
            failed: false,
        })
    }

    pub(crate) fn check(
        &mut self,
        direction: usize,
        bytes: &[u8],
    ) -> Result<(), CaptureSecretError> {
        if self.failed || direction >= self.tails.len() {
            return Err(CaptureSecretError);
        }
        let mut combined = Zeroizing::new(Vec::with_capacity(
            self.tails[direction].len().saturating_add(bytes.len()),
        ));
        combined.extend_from_slice(&self.tails[direction]);
        combined.extend_from_slice(bytes);
        if self.secrets.iter().any(|secret| {
            combined
                .windows(secret.len())
                .any(|part| part == secret.as_slice())
        }) {
            self.failed = true;
            return Err(CaptureSecretError);
        }
        let start = combined.len().saturating_sub(self.max_tail);
        self.tails[direction].zeroize();
        self.tails[direction].extend_from_slice(&combined[start..]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CaptureSecretGuard;

    #[test]
    fn refuses_complete_and_split_credentials_without_crossing_directions() {
        let mut guard = CaptureSecretGuard::new(&[b"gateway-secret", b"provider-token"]).unwrap();
        assert!(guard.check(0, b"ordinary gateway-").is_ok());
        assert!(guard.check(1, b"secret").is_ok());
        assert!(guard.check(0, b"secret").is_err());
        assert!(guard.check(2, b"safe").is_err());
        let mut guard = CaptureSecretGuard::new(&[b"provider-token"]).unwrap();
        assert!(guard.check(3, b"contains provider-token here").is_err());
        assert!(CaptureSecretGuard::new(&[b""]).is_err());
        assert!(CaptureSecretGuard::new(&[&vec![1; 4097]]).is_err());
    }
}
