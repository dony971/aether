//! # Transaction Module
//!
//! Defines the core Transaction structure for the DAG network.
//! Each transaction references two parent transactions, forming a DAG topology.

use serde::{Deserialize, Serialize};
use std::fmt;
use hex;

/// Unique identifier for a transaction (256-bit hash)
pub type TransactionId = [u8; 32];

/// Address type (256-bit)
pub type Address = [u8; 32];

/// Core transaction structure for the DAG
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transaction {
    /// Unique transaction hash (computed from fields)
    pub id: TransactionId,

    /// Two parent transaction IDs (DAG topology)
    pub parents: [TransactionId; 2],

    /// Sender address (derived from public key)
    pub sender: Address,

    /// Receiver address
    pub receiver: Address,

    /// Amount to transfer
    pub amount: u64,

    /// Transaction fee
    pub fee: u64,

    /// Unix timestamp in milliseconds
    pub timestamp: u64,

    /// Nonce for Micro-PoW (anti-spam) - mined to meet difficulty
    pub nonce: u64,

    /// Account nonce for replay protection (sequential per address)
    /// Must be exactly last_nonce + 1 for each transaction from this address
    pub account_nonce: u64,

    /// Cumulative weight (for tip selection) - default 0.0 for CLI compatibility
    #[serde(default)]
    pub weight: f64,

    /// Ed25519 signature (64 bytes)
    pub signature: Vec<u8>,

    /// Ed25519 public key (32 bytes)
    pub public_key: Vec<u8>,
}

impl Transaction {
    /// Create a new transaction
    pub fn new(
        parents: [TransactionId; 2],
        sender: Address,
        receiver: Address,
        amount: u64,
        fee: u64,
        timestamp: u64,
        nonce: u64,
        account_nonce: u64,
        signature: Vec<u8>,
        public_key: Vec<u8>,
    ) -> Self {
        let mut tx = Self {
            id: [0u8; 32],
            parents,
            sender,
            receiver,
            amount,
            fee,
            timestamp,
            nonce,
            account_nonce,
            weight: 0.0,
            signature,
            public_key,
        };

        // Compute hash after all fields are set
        tx.id = tx.compute_hash();
        tx
    }
    
    /// Compute the transaction hash using BLAKE3 (includes signature and public_key)
    pub fn compute_hash(&self) -> TransactionId {
        let mut hasher = blake3::Hasher::new();

        // Hash all fields except id and weight (computed fields)
        hasher.update(&self.parents[0]);
        hasher.update(&self.parents[1]);
        hasher.update(&self.sender);
        hasher.update(&self.receiver);
        hasher.update(&self.amount.to_le_bytes());
        hasher.update(&self.fee.to_le_bytes());
        hasher.update(&self.timestamp.to_le_bytes());
        hasher.update(&self.nonce.to_le_bytes());
        hasher.update(&self.account_nonce.to_le_bytes());
        hasher.update(&self.signature);
        hasher.update(&self.public_key);

        hasher.finalize().into()
    }
    
    /// Compute the signing hash (excludes signature and public_key)
    /// This is what should be signed, not the full hash
    /// Order: Sender + Receiver + Amount + Account Nonce (for replay protection)
    pub fn compute_signing_hash(&self) -> TransactionId {
        let mut hasher = blake3::Hasher::new();

        // Hash only: Sender + Receiver + Amount + Account Nonce
        hasher.update(&self.sender);
        hasher.update(&self.receiver);
        hasher.update(&self.amount.to_le_bytes());
        hasher.update(&self.account_nonce.to_le_bytes());

        hasher.finalize().into()
    }
    
    /// Verify that the transaction hash is valid
    pub fn verify_hash(&self) -> bool {
        self.compute_hash() == self.id
    }
    
    /// Check if this transaction is a genesis transaction (no parents)
    pub fn is_genesis(&self) -> bool {
        self.parents == [TransactionId::default(); 2]
    }
    
    /// Get the total deduction from sender (amount + fee)
    pub fn total_deduction(&self) -> u64 {
        self.amount.saturating_add(self.fee)
    }

    /// Calculate recommended fee based on DAG load and Micro-PoW difficulty
    pub fn calculate_recommended_fee(dag_size: usize, current_tps: u64, difficulty: u64) -> u64 {
        // Base fee
        let base_fee = 1u64;

        // Load factor: increases with DAG size and TPS
        let load_factor = (dag_size as f64 / 10000.0).min(5.0); // Cap at 5x multiplier
        let tps_factor = (current_tps as f64 / 100.0).min(3.0); // Cap at 3x multiplier

        // Difficulty factor: higher difficulty means higher fee for priority
        let difficulty_factor = (difficulty as f64 / 10000.0).min(2.0); // Cap at 2x multiplier

        // Calculate final fee
        let recommended_fee = (base_fee as f64 * (1.0 + load_factor + tps_factor + difficulty_factor)) as u64;

        // Ensure minimum fee of 1
        recommended_fee.max(1)
    }

    /// Deserialize transaction - Manual binary decoding for strict alignment
    /// Matches SDK's serialize() format
    pub fn deserialize(bytes: &[u8]) -> Result<Self, String> {
        const MIN_SIZE: usize = 208; // Fixed fields: 5*32 + 6*8 = 160 + 48 = 208 (added account_nonce)

        if bytes.len() < MIN_SIZE {
            return Err(format!("Data too short: minimum {} bytes required, got {}", MIN_SIZE, bytes.len()));
        }

        let mut cursor = 0;

        // Helper function to safely read slice
        let read_slice = |start: usize, len: usize, field_name: &str| -> Result<&[u8], String> {
            let end = start + len;
            if end > bytes.len() {
                return Err(format!("Erreur: Reçu {} octets, mais j'essaie de lire le champ {} à l'index {} (taille {})", bytes.len(), field_name, start, len));
            }
            Ok(&bytes[start..end])
        };

        // Fixed-size fields (208 bytes total)
        let id: TransactionId = read_slice(cursor, 32, "id")?.try_into()
            .map_err(|_| "Invalid id: cannot convert to [u8; 32]".to_string())?;
        cursor += 32;

        let parent0: TransactionId = read_slice(cursor, 32, "parent0")?.try_into()
            .map_err(|_| "Invalid parent0: cannot convert to [u8; 32]".to_string())?;
        cursor += 32;

        let parent1: TransactionId = read_slice(cursor, 32, "parent1")?.try_into()
            .map_err(|_| "Invalid parent1: cannot convert to [u8; 32]".to_string())?;
        cursor += 32;

        let sender: Address = read_slice(cursor, 32, "sender")?.try_into()
            .map_err(|_| "Invalid sender: cannot convert to [u8; 32]".to_string())?;
        cursor += 32;

        let receiver: Address = read_slice(cursor, 32, "receiver")?.try_into()
            .map_err(|_| "Invalid receiver: cannot convert to [u8; 32]".to_string())?;
        cursor += 32;

        let amount = u64::from_le_bytes(read_slice(cursor, 8, "amount")?.try_into()
            .map_err(|_| "Invalid amount: cannot convert to u64".to_string())?);
        cursor += 8;

        let fee = u64::from_le_bytes(read_slice(cursor, 8, "fee")?.try_into()
            .map_err(|_| "Invalid fee: cannot convert to u64".to_string())?);
        cursor += 8;

        let timestamp = u64::from_le_bytes(read_slice(cursor, 8, "timestamp")?.try_into()
            .map_err(|_| "Invalid timestamp: cannot convert to u64".to_string())?);
        cursor += 8;

        let nonce = u64::from_le_bytes(read_slice(cursor, 8, "nonce")?.try_into()
            .map_err(|_| "Invalid nonce: cannot convert to u64".to_string())?);
        cursor += 8;

        let account_nonce = u64::from_le_bytes(read_slice(cursor, 8, "account_nonce")?.try_into()
            .map_err(|_| "Invalid account_nonce: cannot convert to u64".to_string())?);
        cursor += 8;

        let weight = f64::from_le_bytes(read_slice(cursor, 8, "weight")?.try_into()
            .map_err(|_| "Invalid weight: cannot convert to f64".to_string())?);
        cursor += 8;

        // Variable-size fields (with length prefix)
        let sig_len_bytes = read_slice(cursor, 8, "signature_length")?;
        let sig_len = u64::from_le_bytes(sig_len_bytes.try_into()
            .map_err(|_| "Invalid signature length: cannot convert to u64".to_string())?) as usize;
        cursor += 8;

        let signature = read_slice(cursor, sig_len, "signature")?.to_vec();
        cursor += sig_len;

        let pk_len_bytes = read_slice(cursor, 8, "public_key_length")?;
        let pk_len = u64::from_le_bytes(pk_len_bytes.try_into()
            .map_err(|_| "Invalid public key length: cannot convert to u64".to_string())?) as usize;
        cursor += 8;

        let public_key = read_slice(cursor, pk_len, "public_key")?.to_vec();

        Ok(Self {
            id,
            parents: [parent0, parent1],
            sender,
            receiver,
            amount,
            fee,
            timestamp,
            nonce,
            account_nonce,
            weight,
            signature,
            public_key,
        })
    }

    /// Calculate PoW hash for a transaction with a specific nonce
    /// This is used for mining the nonce to meet difficulty requirements
    pub fn calculate_pow_hash(&self, nonce: u64) -> TransactionId {
        let mut hasher = blake3::Hasher::new();

        // Hash only data fields, NOT signature or public_key
        // This ensures PoW can be computed before signing
        hasher.update(&self.parents[0]);
        hasher.update(&self.parents[1]);
        hasher.update(&self.sender);
        hasher.update(&self.receiver);
        hasher.update(&self.amount.to_le_bytes());
        hasher.update(&self.fee.to_le_bytes());
        hasher.update(&self.timestamp.to_le_bytes());
        hasher.update(&nonce.to_le_bytes());

        hasher.finalize().into()
    }

    /// Verify that the transaction's PoW meets the difficulty requirement
    /// Difficulty is specified as the number of leading zero bits required
    /// For example, difficulty 16 means the hash must start with 16 zero bits (4 hex zeros)
    pub fn verify_pow(&self, difficulty: u8) -> bool {
        let hash = self.calculate_pow_hash(self.nonce);
        
        // Convert difficulty from bits to bytes
        // difficulty 16 = 2 bytes of zeros
        let zero_bytes = (difficulty / 8) as usize;
        let remaining_bits = (difficulty % 8) as u8;
        
        // Check that the first zero_bytes are all zeros
        for i in 0..zero_bytes {
            if hash[i] != 0 {
                return false;
            }
        }
        
        // Check remaining bits in the next byte
        if remaining_bits > 0 && zero_bytes < hash.len() {
            let mask = 0xFF << (8 - remaining_bits);
            if (hash[zero_bytes] & mask) != 0 {
                return false;
            }
        }
        
        true
    }

    /// Mine the nonce to meet the difficulty requirement
    /// Returns the nonce that satisfies the PoW requirement
    pub fn mine_nonce(&self, difficulty: u8) -> u64 {
        let mut nonce: u64 = 0;
        
        loop {
            let hash = self.calculate_pow_hash(nonce);
            
            // Check if hash meets difficulty
            let zero_bytes = (difficulty / 8) as usize;
            let remaining_bits = (difficulty % 8) as u8;
            
            let mut valid = true;
            
            // Check that the first zero_bytes are all zeros
            for i in 0..zero_bytes {
                if hash[i] != 0 {
                    valid = false;
                    break;
                }
            }
            
            // Check remaining bits in the next byte
            if valid && remaining_bits > 0 && zero_bytes < hash.len() {
                let mask = 0xFF << (8 - remaining_bits);
                if (hash[zero_bytes] & mask) != 0 {
                    valid = false;
                }
            }
            
            if valid {
                return nonce;
            }
            
            nonce += 1;
        }
    }

    /// Get the current PoW difficulty (can be adjusted based on network conditions)
    /// Default difficulty is 16 bits (4 hex zeros)
    pub fn default_difficulty() -> u8 {
        16 // 16 leading zero bits = 4 hex zeros (e.g., 0000...)
    }

    /// Verify that the sender address matches the public key
    /// This prevents identity spoofing attacks
    pub fn verify_sender_matches_public_key(&self) -> bool {
        // The sender address should be derived from the public key
        // In Aether, the address is the first 32 bytes of the public key
        if self.public_key.len() < 32 {
            return false;
        }
        
        let derived_address: [u8; 32] = self.public_key[..32].try_into().unwrap_or([0u8; 32]);
        self.sender == derived_address
    }
}

/// Adaptive difficulty tracker for PoW
/// Tracks transaction rates and adjusts difficulty to prevent spam
#[derive(Debug, Clone)]
pub struct AdaptiveDifficulty {
    /// Current difficulty in bits
    current_difficulty: u8,
    
    /// Minimum difficulty
    min_difficulty: u8,
    
    /// Maximum difficulty
    max_difficulty: u8,
    
    /// Transaction timestamps for rate calculation
    tx_timestamps: Vec<u64>,
    
    /// Window size in milliseconds for rate calculation
    window_ms: u64,
    
    /// Target transactions per window
    target_tps: u64,
}

impl AdaptiveDifficulty {
    /// Create a new adaptive difficulty tracker
    pub fn new(min_difficulty: u8, max_difficulty: u8, window_ms: u64, target_tps: u64) -> Self {
        Self {
            current_difficulty: 16, // Start at default difficulty
            min_difficulty,
            max_difficulty,
            tx_timestamps: Vec::new(),
            window_ms,
            target_tps,
        }
    }
    
    /// Create with default parameters
    pub fn default() -> Self {
        Self {
            current_difficulty: 16,
            min_difficulty: 8,  // Minimum 8 bits (2 hex zeros)
            max_difficulty: 24, // Maximum 24 bits (6 hex zeros)
            tx_timestamps: Vec::new(),
            window_ms: 10_000, // 10 second window
            target_tps: 10,    // Target 10 TPS
        }
    }
    
    /// Record a transaction timestamp and adjust difficulty if needed
    pub fn record_transaction(&mut self, timestamp: u64) -> u8 {
        // Add timestamp
        self.tx_timestamps.push(timestamp);
        
        // Remove old timestamps outside window
        let cutoff = timestamp.saturating_sub(self.window_ms);
        self.tx_timestamps.retain(|&ts| ts >= cutoff);
        
        // Calculate current TPS
        let tps = self.tx_timestamps.len() as u64;
        
        // Adjust difficulty based on TPS
        if tps > self.target_tps * 2 {
            // Too many transactions, increase difficulty
            self.current_difficulty = (self.current_difficulty + 1).min(self.max_difficulty);
        } else if tps < self.target_tps / 2 && self.current_difficulty > self.min_difficulty {
            // Too few transactions, decrease difficulty
            self.current_difficulty = (self.current_difficulty - 1).max(self.min_difficulty);
        }
        
        self.current_difficulty
    }
    
    /// Get the current difficulty
    pub fn current_difficulty(&self) -> u8 {
        self.current_difficulty
    }
    
    /// Get the current TPS
    pub fn current_tps(&self) -> u64 {
        self.tx_timestamps.len() as u64
    }
}

impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Transaction[id={}, sender={:?}, receiver={:?}, amount={}, fee={}, timestamp={}]",
            hex::encode(self.id),
            hex::encode(self.sender),
            hex::encode(self.receiver),
            self.amount,
            self.fee,
            self.timestamp
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    #[test]
    fn test_transaction_creation() {
        let parents = [
            [1u8; 32],
            [2u8; 32],
        ];
        let sender = [3u8; 32];
        let receiver = [4u8; 32];
        let signature = vec![5u8; 64];
        let public_key = vec![6u8; 32];

        let tx = Transaction::new(
            parents,
            sender,
            receiver,
            1000,
            10,
            1234567890,
            0,
            1, // account_nonce
            public_key,
            signature,
        );

        assert!(tx.verify_hash());
        assert_eq!(tx.sender, sender);
        assert_eq!(tx.receiver, receiver);
        assert_eq!(tx.amount, 1000);
        assert_eq!(tx.fee, 10);
    }

    #[test]
    fn test_genesis_transaction() {
        let tx = Transaction::new(
            [TransactionId::default(); 2],
            [1u8; 32],
            [2u8; 32],
            0,
            0,
            0,
            0,
            0, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );

        assert!(tx.is_genesis());
    }

    #[test]
    fn test_non_genesis_transaction() {
        let parents = [
            [1u8; 32],
            [2u8; 32],
        ];
        let tx = Transaction::new(
            parents,
            [3u8; 32],
            [4u8; 32],
            100,
            5,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );

        assert!(!tx.is_genesis());
    }

    #[test]
    fn test_total_deduction() {
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            1000,
            50,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );

        assert_eq!(tx.total_deduction(), 1050);
    }

    #[test]
    fn test_hash_deterministic() {
        let parents = [[1u8; 32]; 2];
        let sender = [2u8; 32];
        let receiver = [3u8; 32];
        let signature = vec![4u8; 64];
        let public_key = vec![5u8; 32];

        let tx1 = Transaction::new(parents, sender, receiver, 100, 10, 1234567890, 0, 1, signature.clone(), public_key.clone());
        let tx2 = Transaction::new(parents, sender, receiver, 100, 10, 1234567890, 0, 1, signature, public_key);

        assert_eq!(tx1.id, tx2.id);
    }

    #[test]
    fn test_hash_changes_with_nonce() {
        let parents = [[1u8; 32]; 2];
        let sender = [2u8; 32];
        let receiver = [3u8; 32];
        let signature = vec![4u8; 64];
        let public_key = vec![5u8; 32];

        let tx1 = Transaction::new(parents, sender, receiver, 100, 10, 1234567890, 0, 1, public_key.clone(), signature.clone());
        let tx2 = Transaction::new(parents, sender, receiver, 100, 10, 1234567890, 1, 2, public_key, signature);

        assert_ne!(tx1.id, tx2.id);
    }

    #[test]
    fn test_sender_public_key_match_valid() {
        // Valid case: sender matches public_key (first 32 bytes)
        let public_key = vec![1u8; 64];
        let sender: [u8; 32] = public_key[..32].try_into().expect("Failed to convert public key to sender");
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            sender,
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            public_key,
        );
        
        assert!(tx.verify_sender_matches_public_key());
    }

    #[test]
    fn test_sender_public_key_mismatch_invalid() {
        // Invalid case: sender does NOT match public_key
        let public_key = vec![1u8; 64];
        let sender = [99u8; 32]; // Different from public_key[..32]
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            sender,
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            public_key,
        );
        
        assert!(!tx.verify_sender_matches_public_key());
    }

    #[test]
    fn test_sender_public_key_length_check() {
        // Invalid case: public_key too short
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 16],
            vec![0u8; 64],
        );
        
        assert!(!tx.verify_sender_matches_public_key());
    }

    #[test]
    fn test_identity_spoofing_attack_blocked() {
        // Simulate identity spoofing attack
        let victim_address = [1u8; 32];
        let attacker_public_key = vec![2u8; 64];
        
        let malicious_tx = Transaction::new(
            [[0u8; 32]; 2],
            victim_address,       // Claiming to be victim
            [3u8; 32],
            1000000,              // Large amount
            10,
            1234567890,
            0,
            1, // account_nonce
            attacker_public_key,  // But using attacker's public_key
            vec![0u8; 64],
        );
        
        // V1 fix should block this
        assert!(!malicious_tx.verify_sender_matches_public_key());
    }

    #[test]
    fn test_signing_hash_deterministic() {
        // Test that signing hash is deterministic and matches expected format
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );

        let hash1 = tx.compute_signing_hash();
        let hash2 = tx.compute_signing_hash();
        
        // Should be deterministic
        assert_eq!(hash1, hash2);
        
        // Should be exactly 32 bytes (BLAKE3 output)
        assert_eq!(hash1.len(), 32);
    }
}
