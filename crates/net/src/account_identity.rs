use std::{fmt, sync::Arc};

use master_protocol::MemberId;
use sim::AccountId;

use crypto::KeyPair;

use crate::ConnectionId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountIdentityError {
    Entropy,
    InvalidKey,
    InvalidContext,
    InvalidProof,
}

struct KeyMaterial {
    seed: [u8; 32],
    pair: KeyPair,
}

#[derive(Clone)]
pub struct AccountKey(Arc<KeyMaterial>);

impl fmt::Debug for AccountKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountKey")
            .field("account", &self.account())
            .finish_non_exhaustive()
    }
}

impl AccountKey {
    pub fn generate() -> Result<Self, AccountIdentityError> {
        let mut seed = [0; 32];
        while seed == [0; 32] {
            crypto::fill(&mut seed).map_err(|()| AccountIdentityError::Entropy)?;
        }
        Self::from_seed(seed)
    }

    pub fn from_seed(seed: [u8; 32]) -> Result<Self, AccountIdentityError> {
        if seed == [0; 32] {
            return Err(AccountIdentityError::InvalidKey);
        }
        let pair = KeyPair::from_seed(&seed).ok_or(AccountIdentityError::InvalidKey)?;
        Ok(Self(Arc::new(KeyMaterial { seed, pair })))
    }

    pub fn export_seed(&self) -> [u8; 32] {
        self.0.seed
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.0.pair.public_key()
    }

    pub fn account(&self) -> AccountId {
        account_from_public_key(&self.public_key())
    }

    pub fn prove(&self, challenge: &AccountChallenge, payload: &[u8]) -> AccountProof {
        let account = self.account();
        AccountProof {
            account,
            public_key: self.public_key(),
            signature: self.0.pair.sign(&proof_message(challenge, account, payload)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountChallenge {
    pub match_key: frame::MatchKey,
    pub connection: ConnectionId,
    pub member: MemberId,
    pub nonce: [u8; 32],
}

impl AccountChallenge {
    pub fn new(
        match_key: frame::MatchKey,
        connection: ConnectionId,
        member: MemberId,
    ) -> Result<Self, AccountIdentityError> {
        if match_key.is_none()
            || match_key.session_id == [0; 16]
            || connection.0 == 0
            || member.0 == [0; 16]
        {
            return Err(AccountIdentityError::InvalidContext);
        }
        let mut nonce = [0; 32];
        while nonce == [0; 32] {
            crypto::fill(&mut nonce).map_err(|()| AccountIdentityError::Entropy)?;
        }
        Ok(Self {
            match_key,
            connection,
            member,
            nonce,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountProof {
    pub account: AccountId,
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

impl AccountProof {
    pub fn verify(
        &self,
        challenge: &AccountChallenge,
        payload: &[u8],
    ) -> Result<AccountId, AccountIdentityError> {
        if self.account.0 == [0; 16] || account_from_public_key(&self.public_key) != self.account {
            return Err(AccountIdentityError::InvalidProof);
        }
        if !crypto::verify(
            &self.public_key,
            &proof_message(challenge, self.account, payload),
            &self.signature,
        ) {
            return Err(AccountIdentityError::InvalidProof);
        }
        Ok(self.account)
    }
}

fn account_from_public_key(public_key: &[u8; 32]) -> AccountId {
    let hash = crypto::sha256(&[b"IW4L account identity v1\0", public_key]);
    AccountId(hash[..16].try_into().unwrap())
}

fn proof_message(challenge: &AccountChallenge, account: AccountId, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(160);
    message.extend_from_slice(b"IW4L account proof v1\0");
    message.extend_from_slice(&challenge.match_key.session_id);
    message.extend_from_slice(&challenge.match_key.match_epoch.to_le_bytes());
    message.extend_from_slice(&challenge.connection.0.to_le_bytes());
    message.extend_from_slice(&challenge.member.0);
    message.extend_from_slice(&challenge.nonce);
    message.extend_from_slice(&account.0);
    message.extend_from_slice(&crypto::sha256(&[payload]));
    message
}

#[cfg(not(target_os = "horizon"))]
mod crypto {
    use ring::{
        digest::{self, SHA256},
        rand::{SecureRandom, SystemRandom},
        signature::{self, Ed25519KeyPair, KeyPair as _},
    };

    pub(super) struct KeyPair(Ed25519KeyPair);

    impl KeyPair {
        pub(super) fn from_seed(seed: &[u8; 32]) -> Option<Self> {
            Ed25519KeyPair::from_seed_unchecked(seed).ok().map(Self)
        }

        pub(super) fn public_key(&self) -> [u8; 32] {
            self.0.public_key().as_ref().try_into().unwrap()
        }

        pub(super) fn sign(&self, message: &[u8]) -> [u8; 64] {
            self.0.sign(message).as_ref().try_into().unwrap()
        }
    }

    pub(super) fn fill(out: &mut [u8]) -> Result<(), ()> {
        SystemRandom::new().fill(out).map_err(|_| ())
    }

    pub(super) fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
            .verify(message, signature)
            .is_ok()
    }

    pub(super) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
        let mut hash = digest::Context::new(&SHA256);
        for part in parts {
            hash.update(part);
        }
        hash.finish().as_ref().try_into().unwrap()
    }
}

#[cfg(target_os = "horizon")]
use dalek as crypto;

#[cfg(any(target_os = "horizon", test))]
mod dalek {
    use ed25519_dalek::{Signer as _, Verifier as _};
    use sha2::Digest as _;

    pub(super) struct KeyPair(ed25519_dalek::SigningKey);

    impl KeyPair {
        pub(super) fn from_seed(seed: &[u8; 32]) -> Option<Self> {
            Some(Self(ed25519_dalek::SigningKey::from_bytes(seed)))
        }

        pub(super) fn public_key(&self) -> [u8; 32] {
            self.0.verifying_key().to_bytes()
        }

        pub(super) fn sign(&self, message: &[u8]) -> [u8; 64] {
            self.0.sign(message).to_bytes()
        }
    }

    pub(super) fn fill(out: &mut [u8]) -> Result<(), ()> {
        getrandom::fill(out).map_err(|_| ())
    }

    pub(super) fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        ed25519_dalek::VerifyingKey::from_bytes(public_key).is_ok_and(|key| {
            key.verify(message, &ed25519_dalek::Signature::from_bytes(signature))
                .is_ok()
        })
    }

    pub(super) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
        let mut hash = sha2::Sha256::new();
        for part in parts {
            hash.update(part);
        }
        hash.finalize().into()
    }
}

#[cfg(all(test, not(target_os = "horizon")))]
mod tests {
    use super::{crypto as ring, dalek};

    #[test]
    fn horizon_backend_matches_ring() {
        let message = b"IW4L account proof v1\0payload";
        for seed in [[1u8; 32], [0x5a; 32], core::array::from_fn(|i| i as u8)] {
            let (a, b) = (
                ring::KeyPair::from_seed(&seed).unwrap(),
                dalek::KeyPair::from_seed(&seed).unwrap(),
            );
            assert_eq!(a.public_key(), b.public_key());
            let signature = a.sign(message);
            assert_eq!(signature, b.sign(message));
            assert!(dalek::verify(&b.public_key(), message, &signature));
            assert!(!dalek::verify(&b.public_key(), b"other", &signature));
        }
        let parts: [&[u8]; 2] = [b"IW4L account identity v1\0", &[7; 32]];
        assert_eq!(ring::sha256(&parts), dalek::sha256(&parts));
    }
}
