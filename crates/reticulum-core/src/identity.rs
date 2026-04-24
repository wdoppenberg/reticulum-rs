#[cfg(feature = "alloc")]
use alloc::fmt::Write;
use hkdf::Hkdf;
use rand_core::{CryptoRng, TryCryptoRng};

use ed25519_dalek::{ed25519::signature::Signer, Signature, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret, StaticSecret};

use crate::{
    crypt::fernet::{Fernet, PlainText, Token},
    error::RnsError,
    hash::{AddressHash, Hash},
};

pub const PUBLIC_KEY_LENGTH: usize = ed25519_dalek::PUBLIC_KEY_LENGTH;

#[cfg(feature = "fernet-aes128")]
pub const DERIVED_KEY_LENGTH: usize = 256 / 8;

#[cfg(not(feature = "fernet-aes128"))]
pub const DERIVED_KEY_LENGTH: usize = 512 / 8;

pub trait EncryptIdentity {
    fn encrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        text: &[u8],
        derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError>;
}

pub trait DecryptIdentity {
    fn decrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        data: &[u8],
        derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError>;
}

pub trait HashIdentity {
    fn as_address_hash_slice(&self) -> &[u8];
}

#[derive(Debug, Copy, Clone)]
pub struct Identity {
    pub public_key: PublicKey,
    pub verifying_key: VerifyingKey,
    pub address_hash: AddressHash,
}

impl Identity {
    pub fn new(public_key: PublicKey, verifying_key: VerifyingKey) -> Self {
        let hash = Hash::new(
            Hash::generator()
                .chain_update(public_key.as_bytes())
                .chain_update(verifying_key.as_bytes())
                .finalize()
                .into(),
        );

        let address_hash = AddressHash::new_from_hash(&hash);

        Self {
            public_key,
            verifying_key,
            address_hash,
        }
    }

    /// Construct an `Identity` from raw public-key and verifying-key byte slices.
    ///
    /// Returns `Err(RnsError::CryptoError)` if either slice is not exactly
    /// [`PUBLIC_KEY_LENGTH`] bytes or if the Ed25519 verifying key is invalid,
    /// rather than silently substituting a zero key.
    pub fn new_from_slices(public_key: &[u8], verifying_key: &[u8]) -> Result<Self, RnsError> {
        if public_key.len() != PUBLIC_KEY_LENGTH || verifying_key.len() != PUBLIC_KEY_LENGTH {
            return Err(RnsError::InvalidArgument);
        }

        let public_key = {
            let mut key_data = [0u8; PUBLIC_KEY_LENGTH];
            key_data.copy_from_slice(public_key);
            PublicKey::from(key_data)
        };

        let verifying_key = {
            let mut key_data = [0u8; PUBLIC_KEY_LENGTH];
            key_data.copy_from_slice(verifying_key);
            VerifyingKey::from_bytes(&key_data).map_err(|_| RnsError::CryptoError)?
        };

        Ok(Self::new(public_key, verifying_key))
    }

    pub fn new_from_hex_string(hex_string: &str) -> Result<Self, RnsError> {
        if hex_string.len() < PUBLIC_KEY_LENGTH * 2 * 2 {
            return Err(RnsError::IncorrectHash);
        }

        let mut public_key_bytes = [0u8; PUBLIC_KEY_LENGTH];
        let mut verifying_key_bytes = [0u8; PUBLIC_KEY_LENGTH];

        for i in 0..PUBLIC_KEY_LENGTH {
            public_key_bytes[i] = u8::from_str_radix(&hex_string[i * 2..(i * 2) + 2], 16)
                .map_err(|_| RnsError::IncorrectHash)?;
            verifying_key_bytes[i] = u8::from_str_radix(
                &hex_string[PUBLIC_KEY_LENGTH * 2 + (i * 2)..PUBLIC_KEY_LENGTH * 2 + (i * 2) + 2],
                16,
            )
            .map_err(|_| RnsError::IncorrectHash)?;
        }

        Self::new_from_slices(&public_key_bytes[..], &verifying_key_bytes[..])
    }

    #[cfg(feature = "alloc")]
    pub fn to_hex_string(&self) -> alloc::string::String {
        let mut hex_string = alloc::string::String::with_capacity((PUBLIC_KEY_LENGTH * 2) * 2);

        for byte in self.public_key.as_bytes() {
            write!(&mut hex_string, "{:02x}", byte).unwrap();
        }

        for byte in self.verifying_key.as_bytes() {
            write!(&mut hex_string, "{:02x}", byte).unwrap();
        }

        hex_string
    }

    pub fn public_key_bytes(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        self.public_key.as_bytes()
    }

    pub fn verifying_key_bytes(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        self.verifying_key.as_bytes()
    }

    #[must_use = "a verification failure must be handled"]
    pub fn verify(&self, data: &[u8], signature: &Signature) -> Result<(), RnsError> {
        self.verifying_key
            .verify_strict(data, signature)
            .map_err(|_| RnsError::IncorrectSignature)
    }

    pub fn derive_key<R: CryptoRng + Copy>(&self, rng: R, salt: Option<&[u8]>) -> DerivedKey {
        DerivedKey::new_from_ephemeral_key(rng, &self.public_key, salt)
    }

    pub fn try_derive_key<R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        salt: Option<&[u8]>,
    ) -> Result<DerivedKey, RnsError> {
        DerivedKey::try_new_from_ephemeral_key(rng, &self.public_key, salt)
    }
}

impl Default for Identity {
    fn default() -> Self {
        let empty_key = [0u8; PUBLIC_KEY_LENGTH];
        Self::new(PublicKey::from(empty_key), VerifyingKey::default())
    }
}

impl HashIdentity for Identity {
    fn as_address_hash_slice(&self) -> &[u8] {
        self.address_hash.as_slice()
    }
}

impl EncryptIdentity for Identity {
    fn encrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        text: &[u8],
        derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        let mut out_offset = 0;
        let mut rng = rng;
        let mut ephemeral_secret_bytes = [0u8; PUBLIC_KEY_LENGTH];
        rng.try_fill_bytes(&mut ephemeral_secret_bytes)
            .map_err(|_| RnsError::Randomness)?;
        let ephemeral_key = StaticSecret::from(ephemeral_secret_bytes);
        {
            let ephemeral_public = PublicKey::from(&ephemeral_key);
            let ephemeral_public_bytes = ephemeral_public.as_bytes();

            if out_buf.len() >= ephemeral_public_bytes.len() {
                out_buf[..ephemeral_public_bytes.len()].copy_from_slice(ephemeral_public_bytes);
                out_offset += ephemeral_public_bytes.len();
            } else {
                return Err(RnsError::InvalidArgument);
            }
        }

        let token = Fernet::new_from_slices(
            &derived_key.as_bytes()[..16],
            &derived_key.as_bytes()[16..],
            rng,
        )
        .encrypt(PlainText::from(text), &mut out_buf[out_offset..])?;

        out_offset += token.as_bytes().len();

        Ok(&out_buf[..out_offset])
    }
}

pub struct EmptyIdentity;

impl HashIdentity for EmptyIdentity {
    fn as_address_hash_slice(&self) -> &[u8] {
        &[]
    }
}

impl EncryptIdentity for EmptyIdentity {
    fn encrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        _rng: R,
        text: &[u8],
        _derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        if text.len() > out_buf.len() {
            return Err(RnsError::OutOfMemory);
        }

        let result = &mut out_buf[..text.len()];
        result.copy_from_slice(text);
        Ok(result)
    }
}

impl DecryptIdentity for EmptyIdentity {
    fn decrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        _rng: R,
        data: &[u8],
        _derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        if data.len() > out_buf.len() {
            return Err(RnsError::OutOfMemory);
        }

        let result = &mut out_buf[..data.len()];
        result.copy_from_slice(data);
        Ok(result)
    }
}

/// A Reticulum identity that holds private key material.
///
/// **Not `Clone`**: private key bytes must never be silently duplicated across
/// stack frames.  Key material is zeroed in memory when this value is dropped.
pub struct PrivateIdentity {
    identity: Identity,
    private_key: StaticSecret,
    sign_key: SigningKey,
}

// Both `StaticSecret` (x25519-dalek, "zeroize" feature) and `SigningKey`
// (ed25519-dalek, "zeroize" feature) implement `ZeroizeOnDrop`, meaning their
// own `Drop` impls zero the private bytes.  Because Rust drops struct fields
// in declaration order, the sensitive key material is automatically zeroed
// whenever a `PrivateIdentity` value is dropped — no explicit `Drop` needed.

impl PrivateIdentity {
    pub fn new(private_key: StaticSecret, sign_key: SigningKey) -> Self {
        Self {
            identity: Identity::new((&private_key).into(), sign_key.verifying_key()),
            private_key,
            sign_key,
        }
    }

    pub fn new_from_rand<R: CryptoRng>(mut rng: R) -> Self {
        let sign_key = SigningKey::from_bytes(&Hash::new_from_rand(&mut rng).to_bytes());
        let mut private_key_bytes = [0u8; 32];
        rng.fill_bytes(&mut private_key_bytes);
        let private_key = StaticSecret::from(private_key_bytes);

        Self::new(private_key, sign_key)
    }

    pub fn try_new_from_rand<R: TryCryptoRng>(mut rng: R) -> Result<Self, RnsError> {
        let sign_key = SigningKey::from_bytes(&Hash::try_new_from_rand(&mut rng)?.to_bytes());
        let mut private_key_bytes = [0u8; 32];
        rng.try_fill_bytes(&mut private_key_bytes)
            .map_err(|_| RnsError::Randomness)?;
        let private_key = StaticSecret::from(private_key_bytes);

        Ok(Self::new(private_key, sign_key))
    }

    pub fn new_from_name(name: &str) -> Self {
        let hash = Hash::new_from_slice(name.as_bytes());
        let private_key = StaticSecret::from(hash.to_bytes());

        let hash = Hash::new_from_slice(hash.as_bytes());
        let sign_key = SigningKey::from_bytes(hash.as_bytes());

        Self::new(private_key, sign_key)
    }

    pub fn new_from_hex_string(hex_string: &str) -> Result<Self, RnsError> {
        if hex_string.len() < PUBLIC_KEY_LENGTH * 2 * 2 {
            return Err(RnsError::IncorrectHash);
        }

        let mut private_key_bytes = [0u8; PUBLIC_KEY_LENGTH];
        let mut sign_key_bytes = [0u8; PUBLIC_KEY_LENGTH];

        for i in 0..PUBLIC_KEY_LENGTH {
            private_key_bytes[i] = u8::from_str_radix(&hex_string[i * 2..(i * 2) + 2], 16)
                .map_err(|_| RnsError::IncorrectHash)?;
            sign_key_bytes[i] = u8::from_str_radix(
                &hex_string[PUBLIC_KEY_LENGTH * 2 + (i * 2)..PUBLIC_KEY_LENGTH * 2 + (i * 2) + 2],
                16,
            )
            .map_err(|_| RnsError::IncorrectHash)?;
        }

        Ok(Self::new(
            StaticSecret::from(private_key_bytes),
            SigningKey::from_bytes(&sign_key_bytes),
        ))
    }

    /// Deserialise from 64 raw bytes: `[X25519 private key (32) || Ed25519 seed (32)]`.
    ///
    /// Inverse of [`to_raw_bytes`](Self::to_raw_bytes). Does not require `alloc`.
    pub fn new_from_raw_bytes(raw: &[u8; PUBLIC_KEY_LENGTH * 2]) -> Result<Self, RnsError> {
        let mut priv_bytes = [0u8; PUBLIC_KEY_LENGTH];
        let mut sign_bytes = [0u8; PUBLIC_KEY_LENGTH];
        priv_bytes.copy_from_slice(&raw[..PUBLIC_KEY_LENGTH]);
        sign_bytes.copy_from_slice(&raw[PUBLIC_KEY_LENGTH..]);
        Ok(Self::new(
            StaticSecret::from(priv_bytes),
            SigningKey::from_bytes(&sign_bytes),
        ))
    }

    /// Serialise to 64 raw bytes: `[X25519 private key (32) || Ed25519 seed (32)]`.
    ///
    /// Inverse of [`new_from_raw_bytes`](Self::new_from_raw_bytes). Does not require `alloc`.
    pub fn to_raw_bytes(&self) -> [u8; PUBLIC_KEY_LENGTH * 2] {
        let mut raw = [0u8; PUBLIC_KEY_LENGTH * 2];
        raw[..PUBLIC_KEY_LENGTH].copy_from_slice(self.private_key.as_bytes());
        raw[PUBLIC_KEY_LENGTH..].copy_from_slice(self.sign_key.as_bytes());
        raw
    }

    pub fn sign_key(&self) -> &SigningKey {
        &self.sign_key
    }

    pub fn into(&self) -> &Identity {
        &self.identity
    }

    pub fn as_identity(&self) -> &Identity {
        &self.identity
    }

    pub fn address_hash(&self) -> &AddressHash {
        &self.identity.address_hash
    }

    #[cfg(feature = "alloc")]
    pub fn to_hex_string(&self) -> alloc::string::String {
        let mut hex_string = alloc::string::String::with_capacity((PUBLIC_KEY_LENGTH * 2) * 2);

        for byte in self.private_key.as_bytes() {
            write!(&mut hex_string, "{:02x}", byte).unwrap();
        }

        for byte in self.sign_key.as_bytes() {
            write!(&mut hex_string, "{:02x}", byte).unwrap();
        }

        hex_string
    }

    pub fn verify(&self, data: &[u8], signature: &Signature) -> Result<(), RnsError> {
        self.identity.verify(data, signature)
    }

    pub fn sign(&self, data: &[u8]) -> Result<Signature, RnsError> {
        self.sign_key
            .try_sign(data)
            .map_err(|_| RnsError::CryptoError)
    }

    pub fn exchange(&self, public_key: &PublicKey) -> SharedSecret {
        self.private_key.diffie_hellman(public_key)
    }

    pub fn derive_key(&self, public_key: &PublicKey, salt: Option<&[u8]>) -> DerivedKey {
        DerivedKey::new_from_private_key(&self.private_key, public_key, salt)
    }
}

impl HashIdentity for PrivateIdentity {
    fn as_address_hash_slice(&self) -> &[u8] {
        self.identity.address_hash.as_slice()
    }
}

impl EncryptIdentity for PrivateIdentity {
    fn encrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        text: &[u8],
        derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        let mut out_offset = 0;

        let token = Fernet::new_from_slices(
            &derived_key.as_bytes()[..DERIVED_KEY_LENGTH / 2],
            &derived_key.as_bytes()[DERIVED_KEY_LENGTH / 2..],
            rng,
        )
        .encrypt(PlainText::from(text), &mut out_buf[out_offset..])?;

        out_offset += token.len();

        Ok(&out_buf[..out_offset])
    }
}

impl DecryptIdentity for PrivateIdentity {
    fn decrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        data: &[u8],
        derived_key: &DerivedKey,
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        if data.len() <= PUBLIC_KEY_LENGTH {
            return Err(RnsError::InvalidArgument);
        }

        let fernet = Fernet::new_from_slices(
            &derived_key.as_bytes()[..DERIVED_KEY_LENGTH / 2],
            &derived_key.as_bytes()[DERIVED_KEY_LENGTH / 2..],
            rng,
        );

        let token = Token::from(data);

        let token = fernet.verify(token)?;

        let plain_text = fernet.decrypt(token, out_buf)?;

        Ok(plain_text.as_slice())
    }
}

pub struct GroupIdentity {}

pub struct DerivedKey {
    key: [u8; DERIVED_KEY_LENGTH],
}

impl DerivedKey {
    pub fn new(shared_key: &SharedSecret, salt: Option<&[u8]>) -> Self {
        let mut key = [0u8; DERIVED_KEY_LENGTH];

        let _ = Hkdf::<Sha256>::new(salt, shared_key.as_bytes()).expand(&[], &mut key[..]);

        Self { key }
    }

    pub fn new_empty() -> Self {
        Self {
            key: [0u8; DERIVED_KEY_LENGTH],
        }
    }

    pub fn new_from_private_key(
        priv_key: &StaticSecret,
        pub_key: &PublicKey,
        salt: Option<&[u8]>,
    ) -> Self {
        Self::new(&priv_key.diffie_hellman(pub_key), salt)
    }

    pub fn new_from_ephemeral_key<R: CryptoRng + Copy>(
        rng: R,
        pub_key: &PublicKey,
        salt: Option<&[u8]>,
    ) -> Self {
        let mut rng = rng;
        let secret = EphemeralSecret::random_from_rng(&mut rng);
        let shared_key = secret.diffie_hellman(pub_key);
        Self::new(&shared_key, salt)
    }

    pub fn try_new_from_ephemeral_key<R: TryCryptoRng + Copy>(
        rng: R,
        pub_key: &PublicKey,
        salt: Option<&[u8]>,
    ) -> Result<Self, RnsError> {
        let mut rng = rng;
        let mut secret_bytes = [0u8; PUBLIC_KEY_LENGTH];
        rng.try_fill_bytes(&mut secret_bytes)
            .map_err(|_| RnsError::Randomness)?;
        let secret = StaticSecret::from(secret_bytes);
        let shared_key = secret.diffie_hellman(pub_key);
        Ok(Self::new(&shared_key, salt))
    }

    pub fn as_bytes(&self) -> &[u8; DERIVED_KEY_LENGTH] {
        &self.key
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.key[..]
    }
}

#[cfg(test)]
mod tests {
    use getrandom::SysRng;

    use super::PrivateIdentity;

    #[test]
    fn private_identity_hex_string() {
        let original_id = PrivateIdentity::try_new_from_rand(SysRng).expect("system RNG");
        let original_hex = original_id.to_hex_string();

        let actual_id =
            PrivateIdentity::new_from_hex_string(&original_hex).expect("valid identity");

        assert_eq!(
            actual_id.private_key.as_bytes(),
            original_id.private_key.as_bytes()
        );

        assert_eq!(
            actual_id.sign_key.as_bytes(),
            original_id.sign_key.as_bytes()
        );
    }
}
