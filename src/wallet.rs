//! # Wallet Module
//!
//! Provides cryptographic key generation, signing, and verification for transactions.
//! Uses Ed25519 for digital signatures with BIP39 mnemonic support.

use ed25519_dalek::{Signature, Signer, Verifier};
use ed25519_dalek::SigningKey;
use ed25519_dalek::VerifyingKey;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::fs;
use std::path::Path;
use crate::transaction::{Transaction, Address};
use bip39::{Mnemonic, Language};
use aes_gcm::aead::{Aead, NewAead};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use pbkdf2::pbkdf2_hmac;
use sha2::{Sha256, Digest};

/// Convert public key to human-readable address with checksum
/// Format: AETH<base58_encoded_public_key><checksum>
pub fn public_key_to_address(public_key: &[u8]) -> String {
    // Use first 20 bytes of public key for shorter address
    let key_bytes = &public_key[..20.min(public_key.len())];
    
    // Double SHA256 for checksum
    let hash1 = sha2::Sha256::digest(key_bytes);
    let hash2 = sha2::Sha256::digest(&hash1);
    let checksum = &hash2[..4]; // First 4 bytes as checksum
    
    // Combine key + checksum
    let mut combined = Vec::with_capacity(key_bytes.len() + checksum.len());
    combined.extend_from_slice(key_bytes);
    combined.extend_from_slice(checksum);
    
    // Encode to hex and prefix
    format!("AETH{}", hex::encode(combined))
}

/// Verify address checksum
pub fn verify_address_checksum(address: &str) -> bool {
    if !address.starts_with("AETH") {
        return false;
    }
    
    let hex_part = &address[4..]; // Remove "AETH" prefix
    
    // Decode hex
    let decoded = match hex::decode(hex_part) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    
    if decoded.len() < 24 {
        return false; // 20 bytes key + 4 bytes checksum
    }
    
    let key_bytes = &decoded[..20];
    let checksum = &decoded[20..24];
    
    // Recompute checksum
    let hash1 = sha2::Sha256::digest(key_bytes);
    let hash2 = sha2::Sha256::digest(&hash1);
    let expected_checksum = &hash2[..4];
    
    checksum == expected_checksum
}

/// Encrypted wallet structure for secure storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedWallet {
    /// Encrypted secret key
    pub encrypted_secret: String,
    /// Salt for key derivation
    pub salt: String,
    /// Nonce for AES-GCM
    pub nonce: String,
    /// Public key (not encrypted, for address derivation)
    pub public_key_hex: String,
    /// BIP39 mnemonic (encrypted)
    pub encrypted_mnemonic: String,
}

/// Wallet structure holding the keypair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wallet {
    /// Public key (hex string)
    pub public_key_hex: String,
    /// Secret key (hex string)
    pub secret_key_hex: String,
    /// BIP39 mnemonic phrase
    pub mnemonic: Option<String>,
}

impl Wallet {
    /// Generate a new wallet with a random keypair
    pub fn new() -> Self {
        let mut secret_key_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret_key_bytes);
        let signing_key = SigningKey::from_bytes(&secret_key_bytes);
        let verifying_key = signing_key.verifying_key();
        Wallet {
            public_key_hex: hex::encode(verifying_key.to_bytes()),
            secret_key_hex: hex::encode(signing_key.to_bytes()),
            mnemonic: None,
        }
    }

    /// Generate a new wallet with BIP39 mnemonic
    pub fn new_with_mnemonic() -> Self {
        // Generate random entropy
        let mut entropy = [0u8; 16]; // 128 bits for 12 words
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        
        // Generate mnemonic from entropy
        let mnemonic = Mnemonic::from_entropy(&entropy).expect("Failed to generate mnemonic");
        let mnemonic_phrase = mnemonic.to_string();
        
        // Derive seed from mnemonic using BIP39
        let seed = mnemonic.to_seed("");
        
        // Use first 32 bytes of seed as private key
        let mut secret_key_bytes = [0u8; 32];
        secret_key_bytes.copy_from_slice(&seed[..32]);
        
        let signing_key = SigningKey::from_bytes(&secret_key_bytes);
        let verifying_key = signing_key.verifying_key();
        
        Wallet {
            public_key_hex: hex::encode(verifying_key.to_bytes()),
            secret_key_hex: hex::encode(signing_key.to_bytes()),
            mnemonic: Some(mnemonic_phrase),
        }
    }

    /// Restore wallet from BIP39 mnemonic
    pub fn from_mnemonic(mnemonic_phrase: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mnemonic = Mnemonic::parse_in(Language::English, mnemonic_phrase)?;
        let seed = mnemonic.to_seed("");
        
        let mut secret_key_bytes = [0u8; 32];
        secret_key_bytes.copy_from_slice(&seed[..32]);
        
        let signing_key = SigningKey::from_bytes(&secret_key_bytes);
        let verifying_key = signing_key.verifying_key();
        
        Ok(Wallet {
            public_key_hex: hex::encode(verifying_key.to_bytes()),
            secret_key_hex: hex::encode(signing_key.to_bytes()),
            mnemonic: Some(mnemonic_phrase.to_string()),
        })
    }

    /// Encrypt wallet with password
    pub fn encrypt(&self, password: &str) -> Result<EncryptedWallet, Box<dyn std::error::Error>> {
        // Generate salt
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        
        // Derive key from password using PBKDF2
        let mut key_bytes = [0u8; 32];
        let iterations: u32 = 100_000;
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut key_bytes);
        
        // Generate nonce
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        
        // Encrypt secret key
        let key = Key::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&nonce_bytes);
        
        let secret_bytes = hex::decode(&self.secret_key_hex)?;
        let encrypted_secret = cipher.encrypt(nonce, secret_bytes.as_ref())
            .map_err(|e| format!("Encryption failed: {}", e))?;
        
        // Encrypt mnemonic if present
        let encrypted_mnemonic = if let Some(ref mnemonic) = self.mnemonic {
            cipher.encrypt(nonce, mnemonic.as_bytes())
                .map_err(|e| format!("Mnemonic encryption failed: {}", e))?
        } else {
            vec![]
        };
        
        Ok(EncryptedWallet {
            encrypted_secret: hex::encode(encrypted_secret),
            salt: hex::encode(salt),
            nonce: hex::encode(nonce_bytes),
            public_key_hex: self.public_key_hex.clone(),
            encrypted_mnemonic: hex::encode(encrypted_mnemonic),
        })
    }

    /// Decrypt wallet with password
    pub fn decrypt(encrypted: &EncryptedWallet, password: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Derive key from password
        let salt = hex::decode(&encrypted.salt)?;
        let mut key_bytes = [0u8; 32];
        let iterations: u32 = 100_000;
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut key_bytes);
        
        // Decrypt secret key
        let key = Key::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);
        let nonce_bytes = hex::decode(&encrypted.nonce)?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        
        let encrypted_secret = hex::decode(&encrypted.encrypted_secret)?;
        let secret_bytes = cipher.decrypt(nonce, encrypted_secret.as_ref())
            .map_err(|e| format!("Decryption failed: {}", e))?;
        
        // Decrypt mnemonic if present
        let mnemonic = if !encrypted.encrypted_mnemonic.is_empty() {
            let encrypted_mnemonic = hex::decode(&encrypted.encrypted_mnemonic)?;
            let mnemonic_bytes = cipher.decrypt(nonce, encrypted_mnemonic.as_ref())
                .map_err(|e| format!("Mnemonic decryption failed: {}", e))?;
            Some(String::from_utf8(mnemonic_bytes)?)
        } else {
            None
        };
        
        Ok(Wallet {
            public_key_hex: encrypted.public_key_hex.clone(),
            secret_key_hex: hex::encode(secret_bytes),
            mnemonic,
        })
    }

    /// Load wallet from a file (encrypted)
    pub async fn from_file<P: AsRef<Path>>(path: P, password: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path).await?;
        
        // Try to load as encrypted wallet first
        if let Ok(encrypted) = serde_json::from_str::<EncryptedWallet>(&content) {
            if let Some(pwd) = password {
                return Self::decrypt(&encrypted, pwd);
            } else {
                return Err("Password required for encrypted wallet".into());
            }
        }
        
        // Fallback to unencrypted wallet (legacy)
        let wallet: Wallet = serde_json::from_str(&content)?;
        Ok(wallet)
    }

    /// Save wallet to a file (encrypted if password provided)
    pub async fn to_file<P: AsRef<Path>>(&self, path: P, password: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(pwd) = password {
            let encrypted = self.encrypt(pwd)?;
            let content = serde_json::to_string_pretty(&encrypted)?;
            fs::write(path, content).await?;
        } else {
            let content = serde_json::to_string_pretty(self)?;
            fs::write(path, content).await?;
        }
        Ok(())
    }

    /// Get the address derived from the public key (human-readable format)
    pub fn address(&self) -> Address {
        let public_key_bytes = hex::decode(&self.public_key_hex).unwrap_or_else(|_| {
            tracing::warn!("Failed to decode public key hex in wallet.address()");
            vec![0u8; 32]
        });
        let mut addr = [0u8; 32];
        addr.copy_from_slice(&public_key_bytes[..32.min(public_key_bytes.len())]);
        addr
    }

    /// Get human-readable address string
    pub fn address_string(&self) -> String {
        let public_key_bytes = hex::decode(&self.public_key_hex).unwrap_or_else(|_| {
            tracing::warn!("Failed to decode public key hex in wallet.address_string()");
            vec![0u8; 32]
        });
        public_key_to_address(&public_key_bytes)
    }

    /// Get public key as bytes
    pub fn public_key_bytes(&self) -> Vec<u8> {
        hex::decode(&self.public_key_hex).unwrap_or_else(|_| {
            tracing::warn!("Failed to decode public key hex in wallet.public_key_bytes()");
            vec![0u8; 32]
        })
    }

    /// Get public key as hex string
    pub fn get_public_key(&self) -> String {
        self.public_key_hex.clone()
    }

    /// Get public key as base58 encoded string
    pub fn get_public_key_base58(&self) -> String {
        bs58::encode(&self.public_key_bytes()).into_string()
    }

    /// Sign a transaction hash directly
    pub fn sign_transaction_hash(&self, tx_hash: &[u8]) -> Result<Signature, Box<dyn std::error::Error>> {
        let secret_key_bytes = self.secret_key_bytes();
        let signing_key = SigningKey::try_from(secret_key_bytes.as_slice())?;

        // Sign the transaction hash
        let signature = signing_key.sign(tx_hash);
        Ok(signature)
    }

    /// Get secret key as bytes
    pub fn secret_key_bytes(&self) -> Vec<u8> {
        hex::decode(&self.secret_key_hex).unwrap_or_else(|_| {
            tracing::warn!("Failed to decode secret key hex in wallet.secret_key_bytes()");
            vec![0u8; 64]
        })
    }

    /// Sign a transaction
    pub fn sign_transaction(&self, tx: &Transaction) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let secret_key_bytes = self.secret_key_bytes();
        let signing_key = SigningKey::try_from(secret_key_bytes.as_slice())?;

        // Sign the transaction signing hash (excludes signature and public_key)
        let tx_hash = tx.compute_signing_hash();
        
        let signature = signing_key.sign(&tx_hash);
        Ok(signature.to_bytes().to_vec())
    }

    /// Verify a transaction signature
    pub fn verify_transaction(tx: &Transaction) -> bool {
        let verifying_key: VerifyingKey = match VerifyingKey::try_from(tx.public_key.as_slice()) {
            Ok(pk) => pk,
            Err(_) => {
                tracing::warn!("❌ Invalid public key format");
                return false;
            }
        };

        let signature = match Signature::try_from(tx.signature.as_slice()) {
            Ok(sig) => sig,
            Err(_) => {
                tracing::warn!("❌ Invalid signature format");
                return false;
            }
        };

        // Compute the signing hash (excludes signature and public_key)
        let tx_hash = tx.compute_signing_hash();

        // Verify the signature against the signing hash
        match verifying_key.verify(&tx_hash, &signature) {
            Ok(_) => {
                tracing::info!("✅ Signature verified successfully");
                true
            }
            Err(e) => {
                tracing::warn!("❌ Signature verification failed: {}", e);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallet_generation() {
        let wallet = Wallet::new();
        assert_ne!(wallet.public_key_hex, hex::encode([0u8; 32]));
        assert_ne!(wallet.secret_key_hex, hex::encode([0u8; 32]));
    }

    #[tokio::test]
    async fn test_wallet_persistence() {
        let wallet = Wallet::new();
        let path = "test_wallet.json";

        wallet.to_file(path, None).await.unwrap();
        let loaded = Wallet::from_file(path, None).await.unwrap();

        assert_eq!(wallet.public_key_hex, loaded.public_key_hex);
        assert_eq!(wallet.secret_key_hex, loaded.secret_key_hex);

        // Cleanup
        tokio::fs::remove_file(path).await.ok();
    }

    #[test]
    fn test_sign_verify() {
        let wallet = Wallet::new();
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            wallet.address(),
            [1u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            wallet.public_key_bytes(),
        );

        let signature = wallet.sign_transaction(&tx).unwrap();
        let mut signed_tx = tx.clone();
        signed_tx.signature = signature;

        assert!(Wallet::verify_transaction(&signed_tx));
    }

    #[test]
    fn test_invalid_signature() {
        let wallet = Wallet::new();
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            wallet.address(),
            [1u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![99u8; 64], // Invalid signature
            wallet.public_key_bytes(),
        );

        assert!(!Wallet::verify_transaction(&tx));
    }

    #[tokio::test]
    async fn test_sign_verify_with_persistence() {
        let wallet = Wallet::new();
        let path = "test_wallet_persistence.json";

        // Save wallet
        wallet.to_file(path, None).await.unwrap();

        // Load wallet
        let loaded_wallet = Wallet::from_file(path, None).await.unwrap();

        // Create test transaction
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            loaded_wallet.address(),
            [1u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            loaded_wallet.public_key_bytes(),
        );

        // Sign with loaded wallet
        let signature = loaded_wallet.sign_transaction(&tx).unwrap();
        let mut signed_tx = tx.clone();
        signed_tx.signature = signature;

        // Verify signature
        assert!(Wallet::verify_transaction(&signed_tx));

        // Cleanup
        tokio::fs::remove_file(path).await.ok();
    }

    #[tokio::test]
    async fn test_generate_save_load_sign_loop() {
        // Generate wallet
        let wallet = Wallet::new();
        let path = "test_wallet_loop.json";

        // Save wallet
        wallet.to_file(path, None).await.unwrap();

        // Load wallet
        let loaded_wallet = Wallet::from_file(path, None).await.unwrap();

        // Create test transaction
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            loaded_wallet.address(),
            [1u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            loaded_wallet.public_key_bytes(),
        );

        // Sign with loaded wallet
        let signature = loaded_wallet.sign_transaction(&tx).unwrap();
        let mut signed_tx = tx.clone();
        signed_tx.signature = signature;

        // Verify signature
        assert!(Wallet::verify_transaction(&signed_tx));

        // Cleanup
        tokio::fs::remove_file(path).await.ok();
    }
}
