use crate::capture_key_usage::WrapPermit;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_BLOCK_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct CaptureIdentity {
    pub tenant: Uuid,
    pub capture: Uuid,
    pub user: Uuid,
    pub created_ms: i64,
    pub retention_days: u16,
}

#[derive(Clone, Copy)]
pub(crate) enum CaptureLeg {
    ClientRequest,
    ProviderRequest,
    ProviderResponse,
    ClientResponse,
    Manifest,
}

impl CaptureLeg {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::ClientRequest => 0,
            Self::ProviderRequest => 1,
            Self::ProviderResponse => 2,
            Self::ClientResponse => 3,
            Self::Manifest => 4,
        }
    }

    pub(crate) fn from_index(index: u8) -> Result<Self, CaptureCryptoError> {
        match index {
            0 => Ok(Self::ClientRequest),
            1 => Ok(Self::ProviderRequest),
            2 => Ok(Self::ProviderResponse),
            3 => Ok(Self::ClientResponse),
            4 => Ok(Self::Manifest),
            _ => Err(CaptureCryptoError::Invalid),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureCryptoError {
    #[error("capture encryption failed")]
    Invalid,
}

pub(crate) struct CaptureCipher {
    identity: CaptureIdentity,
    key: LessSafeKey,
    wrapped: Vec<u8>,
    fingerprint: [u8; 32],
}

impl CaptureCipher {
    pub fn create(
        master: &[u8; 32],
        identity: CaptureIdentity,
        permit: WrapPermit,
    ) -> Result<Self, CaptureCryptoError> {
        if !permit.matches(master) {
            return Err(CaptureCryptoError::Invalid);
        }
        let mut data_key = Zeroizing::new([0u8; 32]);
        getrandom::fill(data_key.as_mut()).map_err(|_| CaptureCryptoError::Invalid)?;
        let master_key = key(master)?;
        let wrapped = encrypt(&master_key, &identity_aad(identity), data_key.as_ref())?;
        Ok(Self {
            identity,
            key: key(data_key.as_ref())?,
            wrapped,
            fingerprint: Sha256::digest(master).into(),
        })
    }

    pub fn open(
        master: &[u8; 32],
        identity: CaptureIdentity,
        wrapped: &[u8],
    ) -> Result<Self, CaptureCryptoError> {
        if wrapped.len() != NONCE_BYTES + 32 + TAG_BYTES {
            return Err(CaptureCryptoError::Invalid);
        }
        let data_key = Zeroizing::new(decrypt(&key(master)?, &identity_aad(identity), wrapped)?);
        Ok(Self {
            identity,
            key: key(data_key.as_ref())?,
            wrapped: wrapped.to_vec(),
            fingerprint: Sha256::digest(master).into(),
        })
    }

    pub fn wrapped_key(&self) -> &[u8] {
        &self.wrapped
    }

    pub fn fingerprint(&self) -> &[u8; 32] {
        &self.fingerprint
    }

    pub fn identity(&self) -> CaptureIdentity {
        self.identity
    }

    pub fn seal(
        &self,
        leg: CaptureLeg,
        sequence: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CaptureCryptoError> {
        if plaintext.len() > MAX_BLOCK_BYTES {
            return Err(CaptureCryptoError::Invalid);
        }
        encrypt(
            &self.key,
            &block_aad(self.identity, leg, sequence),
            plaintext,
        )
    }

    pub fn unseal(
        &self,
        leg: CaptureLeg,
        sequence: u64,
        block: &[u8],
    ) -> Result<Vec<u8>, CaptureCryptoError> {
        if block.len() > MAX_BLOCK_BYTES + NONCE_BYTES + TAG_BYTES {
            return Err(CaptureCryptoError::Invalid);
        }
        decrypt(&self.key, &block_aad(self.identity, leg, sequence), block)
    }

    pub fn seal_timed(
        &self,
        leg: CaptureLeg,
        sequence: u64,
        position: u64,
        elapsed_ns: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CaptureCryptoError> {
        if plaintext.len() > MAX_BLOCK_BYTES {
            return Err(CaptureCryptoError::Invalid);
        }
        encrypt(
            &self.key,
            &timed_aad(self.identity, leg, sequence, position, elapsed_ns),
            plaintext,
        )
    }

    pub fn unseal_timed(
        &self,
        leg: CaptureLeg,
        sequence: u64,
        position: u64,
        elapsed_ns: u64,
        block: &[u8],
    ) -> Result<Vec<u8>, CaptureCryptoError> {
        if block.len() > MAX_BLOCK_BYTES + NONCE_BYTES + TAG_BYTES {
            return Err(CaptureCryptoError::Invalid);
        }
        decrypt(
            &self.key,
            &timed_aad(self.identity, leg, sequence, position, elapsed_ns),
            block,
        )
    }
}

fn key(bytes: &[u8]) -> Result<LessSafeKey, CaptureCryptoError> {
    UnboundKey::new(&aead::AES_256_GCM, bytes)
        .map(LessSafeKey::new)
        .map_err(|_| CaptureCryptoError::Invalid)
}

fn identity_aad(identity: CaptureIdentity) -> Vec<u8> {
    let mut value = b"gateway capture key v1".to_vec();
    value.extend_from_slice(identity.tenant.as_bytes());
    value.extend_from_slice(identity.capture.as_bytes());
    value.extend_from_slice(identity.user.as_bytes());
    value.extend_from_slice(&identity.created_ms.to_be_bytes());
    value.extend_from_slice(&identity.retention_days.to_be_bytes());
    value
}

fn block_aad(identity: CaptureIdentity, leg: CaptureLeg, sequence: u64) -> Vec<u8> {
    let mut value = b"gateway capture block v1".to_vec();
    value.extend_from_slice(identity.tenant.as_bytes());
    value.extend_from_slice(identity.capture.as_bytes());
    value.extend_from_slice(identity.user.as_bytes());
    value.extend_from_slice(&identity.created_ms.to_be_bytes());
    value.extend_from_slice(&identity.retention_days.to_be_bytes());
    value.push(leg.index() as u8);
    value.extend_from_slice(&sequence.to_be_bytes());
    value
}

fn timed_aad(
    identity: CaptureIdentity,
    leg: CaptureLeg,
    sequence: u64,
    position: u64,
    elapsed_ns: u64,
) -> Vec<u8> {
    let mut value = block_aad(identity, leg, sequence);
    value.extend_from_slice(&position.to_be_bytes());
    value.extend_from_slice(&elapsed_ns.to_be_bytes());
    value
}

fn encrypt(key: &LessSafeKey, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CaptureCryptoError> {
    let mut nonce = [0u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| CaptureCryptoError::Invalid)?;
    let mut sealed = plaintext.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad),
        &mut sealed,
    )
    .map_err(|_| CaptureCryptoError::Invalid)?;
    let mut output = nonce.to_vec();
    output.extend_from_slice(&sealed);
    Ok(output)
}

fn decrypt(key: &LessSafeKey, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CaptureCryptoError> {
    if sealed.len() < NONCE_BYTES + TAG_BYTES {
        return Err(CaptureCryptoError::Invalid);
    }
    let nonce: [u8; NONCE_BYTES] = sealed[..NONCE_BYTES]
        .try_into()
        .map_err(|_| CaptureCryptoError::Invalid)?;
    let mut body = sealed[NONCE_BYTES..].to_vec();
    let length = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut body,
        )
        .map_err(|_| CaptureCryptoError::Invalid)?
        .len();
    body.truncate(length);
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{CaptureCipher, CaptureIdentity, CaptureLeg};
    use uuid::Uuid;

    #[test]
    fn encrypted_blocks_reject_tampering_and_cross_request_reads() {
        let master = [7u8; 32];
        let identity = CaptureIdentity {
            tenant: Uuid::new_v4(),
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        let cipher = CaptureCipher::create(
            &master,
            identity,
            crate::capture_key_usage::WrapPermit::for_test(&master),
        )
        .unwrap();
        let wrapped = cipher.wrapped_key();
        let block = cipher
            .seal(CaptureLeg::ClientRequest, 0, b"private prompt")
            .unwrap();
        assert!(
            !block
                .windows(b"private prompt".len())
                .any(|value| value == b"private prompt")
        );
        let reopened = CaptureCipher::open(&master, identity, wrapped).unwrap();
        assert_eq!(
            reopened
                .unseal(CaptureLeg::ClientRequest, 0, &block)
                .unwrap(),
            b"private prompt"
        );
        assert!(
            reopened
                .unseal(CaptureLeg::ClientResponse, 0, &block)
                .is_err()
        );
        assert!(
            reopened
                .unseal(CaptureLeg::ClientRequest, 1, &block)
                .is_err()
        );
        let mut damaged = block.clone();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(
            reopened
                .unseal(CaptureLeg::ClientRequest, 0, &damaged)
                .is_err()
        );
        let other = CaptureIdentity {
            tenant: identity.tenant,
            capture: Uuid::new_v4(),
            user: Uuid::new_v4(),
            created_ms: chrono::Utc::now().timestamp_millis(),
            retention_days: 30,
        };
        assert!(CaptureCipher::open(&master, other, wrapped).is_err());
        assert!(CaptureCipher::open(&[8u8; 32], identity, wrapped).is_err());
        let timed = cipher
            .seal_timed(CaptureLeg::ClientRequest, 0, 3, 200, b"frame")
            .unwrap();
        assert!(
            cipher
                .unseal_timed(CaptureLeg::ClientRequest, 0, 3, 200, &timed)
                .is_ok()
        );
        assert!(
            cipher
                .unseal_timed(CaptureLeg::ClientRequest, 0, 4, 200, &timed)
                .is_err()
        );
        assert!(
            cipher
                .unseal_timed(CaptureLeg::ClientRequest, 0, 3, 201, &timed)
                .is_err()
        );
        for leg in [
            CaptureLeg::ClientRequest,
            CaptureLeg::ProviderRequest,
            CaptureLeg::ProviderResponse,
            CaptureLeg::ClientResponse,
        ] {
            let first = cipher.seal(leg, 2, b"frame").unwrap();
            let second = cipher.seal(leg, 2, b"frame").unwrap();
            assert_ne!(first, second);
            assert_eq!(reopened.unseal(leg, 2, &first).unwrap(), b"frame");
        }
        assert!(
            reopened
                .unseal(CaptureLeg::ClientRequest, 0, &block[..10])
                .is_err()
        );
        assert!(
            cipher
                .seal(CaptureLeg::ClientRequest, 0, &vec![0; 1024 * 1024 + 1])
                .is_err()
        );
    }
}
