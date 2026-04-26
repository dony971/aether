//! # Genesis Module
//!
//! Handles genesis initialization for the Aether blockchain.
//! Generates the first Epoch of the DAG with a timestamped genesis message.

use crate::transaction::{Transaction, TransactionId, Address};
use crate::parent_selection::DAG;
use crate::consensus::VQVConsensus;
use std::collections::HashMap;
use hex;

/// Genesis block ID (hash of 64 zeros)
pub const GENESIS_HASH: TransactionId = [0u8; 32];

/// Genesis message containing a news headline to prove launch date
/// "23/Apr/2026 - Aether: Trust is computed, not granted. Le Monde 21/04/2026: L'Aether naît du chaos numérique."
pub const GENESIS_MESSAGE: &str = "23/Apr/2026 - Aether: Trust is computed, not granted. Le Monde 21/04/2026: L'Aether naît du chaos numérique.";

/// Aether Founder address (receives 1M AETH at genesis)
/// Derived from public key: 1de352e44cd333672593f2334a730e180aaf290de89aa16d480de594e34e2961
pub const FOUNDER_ADDRESS: &str = "3d17ace653283dbd9aeba6e0d4684795a800e9da952cb682bb67cd970cbe1b3e";

/// Genesis ledger with initial token distribution
/// 10 AETH = 10,000,000,000,000,000,000 units (18 decimals)
/// Note: u64 max is ~18.44 AETH with 18 decimals
pub const GENESIS_LEDGER: [(&str, u64); 1] = [
    (FOUNDER_ADDRESS, 10_000_000_000_000_000_000_u64), // 10 AETH for founder
];

/// Genesis configuration
#[derive(Debug, Clone)]
pub struct GenesisConfig {
    /// Genesis timestamp
    pub timestamp: u64,
    
    /// Initial difficulty
    pub initial_difficulty: u64,
    
    /// Initial validators
    pub initial_validators: Vec<Address>,
    
    /// Initial token distribution (address -> balance)
    pub initial_balances: HashMap<Address, u64>,
}

impl Default for GenesisConfig {
    fn default() -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();
        
        // Use GENESIS_LEDGER for initial token distribution
        let mut initial_balances = HashMap::new();
        for (address_hex, balance) in GENESIS_LEDGER {
            let addr_bytes = hex::decode(address_hex)
                .expect("Invalid genesis address hex");
            let mut addr = [0u8; 32];
            addr.copy_from_slice(&addr_bytes);
            tracing::info!("🔍 GenesisConfig: Adding address {} with balance {} (raw)", address_hex, balance);
            initial_balances.insert(addr, balance);
        }
        
        // Initial validators (use founder as initial validator)
        let founder_addr = hex::decode(FOUNDER_ADDRESS)
            .expect("Invalid founder address hex");
        let mut founder = [0u8; 32];
        founder.copy_from_slice(&founder_addr);
        let initial_validators = vec![founder];
        
        Self {
            timestamp,
            initial_difficulty: 1000, // Minimal initial difficulty
            initial_validators,
            initial_balances,
        }
    }
}

/// Genesis block (first transaction in DAG)
#[derive(Debug, Clone)]
pub struct GenesisBlock {
    /// Genesis transaction
    pub transaction: Transaction,
    
    /// Genesis message
    pub message: String,
    
    /// Genesis configuration
    pub config: GenesisConfig,
}

impl GenesisBlock {
    /// Create new genesis block
    pub fn new(config: GenesisConfig) -> Self {
        // Genesis address (same as in main.rs ledger initialization)
        let genesis_address_bytes = hex::decode("e6e05920156d3184ace93cc8fcc34c6be69c55e903b34b2b614452a9a1a8a398")
            .expect("Invalid genesis hex");
        let genesis_address: Address = genesis_address_bytes.try_into()
            .expect("Invalid genesis address length");

        // Create genesis transaction (no parents)
        let transaction = Transaction::new(
            [[0u8; 32]; 2], // No parents (genesis)
            [0u8; 32],     // Genesis sender (null address)
            genesis_address, // Genesis receiver (actual genesis address)
            0,             // Amount
            0,             // Fee
            config.timestamp,
            0,             // No PoW nonce
            0,             // No account_nonce (genesis doesn't use replay protection)
            vec![0u8; 64], // Signature (genesis has no signature)
            vec![0u8; 32], // Public key (genesis has no public key)
        );

        Self {
            transaction,
            message: GENESIS_MESSAGE.to_string(),
            config,
        }
    }
    
    /// Get genesis transaction ID
    pub fn genesis_id(&self) -> TransactionId {
        self.transaction.id
    }
    
    /// Verify genesis block
    pub fn verify(&self) -> bool {
        // Verify genesis message is embedded
        // In production, this would cryptographically embed the message
        self.message == GENESIS_MESSAGE
    }
}

/// Initialize genesis state
pub fn initialize_genesis(config: GenesisConfig) -> (DAG, VQVConsensus, HashMap<String, u64>, std::collections::HashMap<[u8; 32], crate::transaction::Transaction>, Vec<Vec<u8>>) {
    // Create genesis block
    let genesis = GenesisBlock::new(config.clone());

    // Initialize DAG with genesis transaction
    let mut dag = DAG::new();
    // Genesis transaction is pre-validated, use add_transaction_validated
    dag.add_transaction_validated(genesis.transaction.clone()).expect("Genesis transaction should be valid");

    // Initialize consensus with initial validators
    let consensus = VQVConsensus::new(
        config.initial_validators.len(), // quorum_size
        0.67, // approval_threshold (2/3)
        1_000_000, // min_stake
        10_000_000, // max_stake
    );

    // Initialize balances from config
    let balances: HashMap<String, u64> = config.initial_balances
        .into_iter()
        .map(|(addr, bal)| (hex::encode(addr), bal))
        .collect();

    (dag, consensus, balances, std::collections::HashMap::new(), Vec::new())
}

/// Get genesis block hash (for verification)
pub fn genesis_hash(config: &GenesisConfig) -> String {
    let genesis = GenesisBlock::new(config.clone());
    hex::encode(genesis.genesis_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_creation() {
        let config = GenesisConfig::default();
        let genesis = GenesisBlock::new(config);
        
        assert_eq!(genesis.message, GENESIS_MESSAGE);
        assert!(genesis.verify());
    }

    #[test]
    fn test_genesis_initialization() {
        let config = GenesisConfig::default();
        let (dag, consensus, balances, _txs, _addrs) = initialize_genesis(config);
        
        assert_eq!(dag.transaction_count(), 1);
        assert!(!balances.is_empty());
    }

    #[test]
    fn test_genesis_hash() {
        let config = GenesisConfig::default();
        let hash = genesis_hash(&config);
        
        assert_eq!(hash.len(), 64); // 32 bytes = 64 hex chars
    }
}
