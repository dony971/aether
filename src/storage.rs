//! # Storage Module
//!
//! Implements Sled persistence for transactions, DAG state, and wallet balances.
//! Includes atomic writes, separate trees for balances and transactions, and migration from JSON.

use crate::transaction::{Transaction, TransactionId, Address};
use sled::{Db, Tree, Transactional};
use std::path::Path;
use std::collections::HashMap;
use thiserror::Error;
use serde::{Serialize, Deserialize};

/// Storage error types
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Sled error: {0}")]
    Sled(#[from] sled::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),

    #[error("Transaction not found")]
    TransactionNotFound,

    #[error("Key not found")]
    KeyNotFound,

    #[error("Database error: {0}")]
    DatabaseError(String),
}

impl From<sled::transaction::TransactionError> for StorageError {
    fn from(e: sled::transaction::TransactionError) -> Self {
        StorageError::DatabaseError(format!("Transaction error: {}", e))
    }
}

impl From<Box<dyn std::error::Error>> for StorageError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        StorageError::DatabaseError(e.to_string())
    }
}

/// Tree names for separate data stores
pub enum TreeName {
    Transactions,
    Balances,
    Metadata,
    AddressIndex, // Index transactions by sender/receiver address
    Staking,      // Staking positions and rewards
    Nonces,       // Account nonces for replay protection
    Orphans,      // Orphan transactions waiting for parents
}

impl TreeName {
    fn name(&self) -> &'static str {
        match self {
            TreeName::Transactions => "transactions",
            TreeName::Balances => "balances",
            TreeName::Metadata => "metadata",
            TreeName::AddressIndex => "address_index",
            TreeName::Staking => "staking",
            TreeName::Nonces => "nonces",
            TreeName::Orphans => "orphans",
        }
    }
}

/// Storage backend using Sled
#[derive(Debug)]
pub struct Storage {
    db: Db,
    transactions: Tree,
    balances: Tree,
    metadata: Tree,
    address_index: Tree,
    staking: Tree,
    nonces: Tree,
    orphans: Tree,
}

/// Staking position structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingPosition {
    /// Address of the staker
    pub address: Address,
    /// Amount staked (in base units, 18 decimals)
    pub staked_amount: u64,
    /// Timestamp when staking started
    pub start_timestamp: u64,
    /// Total rewards earned so far
    pub rewards_earned: u64,
    /// Last rewards calculation timestamp
    pub last_reward_timestamp: u64,
}

impl Storage {
    /// Open or create a database at the given path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        let db = sled::open(path)?;
        
        // Open or create trees
        let transactions = db.open_tree(TreeName::Transactions.name())?;
        let balances = db.open_tree(TreeName::Balances.name())?;
        let metadata = db.open_tree(TreeName::Metadata.name())?;
        let address_index = db.open_tree(TreeName::AddressIndex.name())?;
        let staking = db.open_tree(TreeName::Staking.name())?;
        let nonces = db.open_tree(TreeName::Nonces.name())?;
        let orphans = db.open_tree(TreeName::Orphans.name())?;
        
        Ok(Self {
            db,
            transactions,
            balances,
            metadata,
            address_index,
            staking,
            nonces,
            orphans,
        })
    }

    /// Get a tree by name
    fn tree(&self, tree_name: TreeName) -> &Tree {
        match tree_name {
            TreeName::Transactions => &self.transactions,
            TreeName::Balances => &self.balances,
            TreeName::Metadata => &self.metadata,
            TreeName::AddressIndex => &self.address_index,
            TreeName::Staking => &self.staking,
            TreeName::Nonces => &self.nonces,
            TreeName::Orphans => &self.orphans,
        }
    }
    
    /// Store a transaction
    pub fn put_transaction(&self, tx: &Transaction) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Transactions);
        let key = tx.id;
        let value = bincode::serialize(tx)?;
        tree.insert(key, value)?;
        
        // Index by sender address for O(1) lookup
        let index_tree = self.tree(TreeName::AddressIndex);
        let mut sender_key = Vec::with_capacity(64);
        sender_key.extend_from_slice(&tx.sender);
        sender_key.extend_from_slice(&tx.id);
        index_tree.insert(sender_key, tx.id.to_vec())?;
        
        // Index by receiver address for O(1) lookup
        let mut receiver_key = Vec::with_capacity(64);
        receiver_key.extend_from_slice(&tx.receiver);
        receiver_key.extend_from_slice(&tx.id);
        index_tree.insert(receiver_key, tx.id.to_vec())?;
        
        Ok(())
    }

    /// Get a transaction by ID
    pub fn get_transaction(&self, id: TransactionId) -> Result<Transaction, StorageError> {
        let tree = self.tree(TreeName::Transactions);
        let value = tree.get(id)?.ok_or(StorageError::TransactionNotFound)?;
        let tx: Transaction = bincode::deserialize(&value)?;
        Ok(tx)
    }

    /// Check if a transaction exists
    pub fn transaction_exists(&self, id: TransactionId) -> Result<bool, StorageError> {
        let tree = self.tree(TreeName::Transactions);
        Ok(tree.get(id)?.is_some())
    }

    /// Delete a transaction
    pub fn delete_transaction(&self, id: TransactionId) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Transactions);
        tree.remove(id)?;
        Ok(())
    }

    /// Get all transactions
    pub fn get_all_transactions(&self) -> Result<Vec<Transaction>, StorageError> {
        let tree = self.tree(TreeName::Transactions);
        let mut transactions = Vec::new();

        for item in tree.iter() {
            let (_, value) = item.map_err(|e| StorageError::DatabaseError(e.to_string()))?;
            let tx: Transaction = bincode::deserialize(&value)?;
            transactions.push(tx);
        }

        Ok(transactions)
    }
    
    /// Get transactions by address (O(1) lookup using index)
    pub fn get_transactions_by_address(&self, address: &Address) -> Result<Vec<Transaction>, StorageError> {
        let index_tree = self.tree(TreeName::AddressIndex);
        let mut transactions = Vec::new();
        
        // Scan index for this address prefix
        let prefix = address.as_ref();
        for item in index_tree.scan_prefix(prefix) {
            let (_, tx_id) = item.map_err(|e| StorageError::DatabaseError(e.to_string()))?;
            let tx_id_array: TransactionId = tx_id.as_ref().try_into()
                .map_err(|_| StorageError::DatabaseError("Invalid transaction ID length".to_string()))?;
            
            if let Ok(tx) = self.get_transaction(tx_id_array) {
                transactions.push(tx);
            }
        }
        
        Ok(transactions)
    }
    
    /// Store a wallet balance
    pub fn put_balance(&self, address: Address, balance: u64) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Balances);
        let value = balance.to_le_bytes().to_vec();
        tree.insert(address, value)?;
        Ok(())
    }

    /// Get a wallet balance
    pub fn get_balance(&self, address: Address) -> Result<u64, StorageError> {
        let tree = self.tree(TreeName::Balances);
        let value = tree.get(address)?.ok_or(StorageError::KeyNotFound)?;
        let balance = u64::from_le_bytes(value.as_ref().try_into()
            .map_err(|_| StorageError::DatabaseError("Invalid balance value length".to_string()))?);
        Ok(balance)
    }

    /// Get all balances
    pub fn get_all_balances(&self) -> Result<HashMap<Address, u64>, StorageError> {
        let tree = self.tree(TreeName::Balances);
        let mut balances = HashMap::new();

        for item in tree.iter() {
            let (key, value) = item.map_err(|e| StorageError::DatabaseError(e.to_string()))?;
            let address: Address = key.as_ref().try_into()
                .map_err(|_| StorageError::DatabaseError("Invalid address length".to_string()))?;
            let balance = u64::from_le_bytes(value.as_ref().try_into()
                .map_err(|_| StorageError::DatabaseError("Invalid balance value length".to_string()))?);
            balances.insert(address, balance);
        }

        Ok(balances)
    }
    
    /// Store metadata
    pub fn put_metadata(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Metadata);
        tree.insert(key, value)?;
        Ok(())
    }

    /// Get metadata
    pub fn get_metadata(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let tree = self.tree(TreeName::Metadata);
        let value = tree.get(key)?.ok_or(StorageError::KeyNotFound)?;
        Ok(value.to_vec())
    }

    /// Batch write operations (atomic using Sled transaction)
    /// All operations are wrapped in a single Sled transaction to guarantee
    /// that a crash cannot leave partial data
    pub fn batch_write(&self, operations: Vec<BatchOperation>) -> Result<(), StorageError> {
        // Prepare data for transaction closure
        let mut tx_inserts: Vec<(TransactionId, Vec<u8>)> = Vec::new();
        let mut tx_deletes: Vec<TransactionId> = Vec::new();
        let mut balance_updates: Vec<([u8; 32], Vec<u8>)> = Vec::new();
        let mut index_inserts: Vec<(Vec<u8>, TransactionId)> = Vec::new();
        
        for op in operations {
            match op {
                BatchOperation::PutTransaction(tx) => {
                    let key = tx.id;
                    let value = bincode::serialize(&tx)?;
                    tx_inserts.push((key, value));
                    
                    // Add to index
                    let mut sender_key = Vec::with_capacity(64);
                    sender_key.extend_from_slice(&tx.sender);
                    sender_key.extend_from_slice(&tx.id);
                    let mut receiver_key = Vec::with_capacity(64);
                    receiver_key.extend_from_slice(&tx.receiver);
                    receiver_key.extend_from_slice(&tx.id);
                    index_inserts.push((sender_key, tx.id));
                    index_inserts.push((receiver_key, tx.id));
                }
                BatchOperation::DeleteTransaction(id) => {
                    tx_deletes.push(id);
                }
                BatchOperation::PutBalance(address, balance) => {
                    let value = balance.to_le_bytes().to_vec();
                    balance_updates.push((address, value));
                }
            }
        }
        
        // Execute all operations in a single Sled transaction
        let tx_tree = self.transactions.clone();
        let balance_tree = self.balances.clone();
        let index_tree = self.address_index.clone();
        
        (&tx_tree, &balance_tree, &index_tree).transaction(|(tx_tree, balance_tree, index_tree)| {
            // Transaction inserts
            for (key, value) in &tx_inserts {
                tx_tree.insert(key.as_ref(), value.as_slice())?;
            }
            
            // Transaction deletes
            for id in &tx_deletes {
                tx_tree.remove(id.as_ref())?;
            }
            
            // Balance updates
            for (address, value) in &balance_updates {
                balance_tree.insert(address.as_ref(), value.as_slice())?;
            }
            
            // Index updates
            for (key, tx_id) in &index_inserts {
                index_tree.insert(key.as_slice(), tx_id.as_ref())?;
            }
            
            Ok::<(), sled::transaction::ConflictableTransactionError<StorageError>>(())
        }).map_err(|e| StorageError::DatabaseError(format!("Sled transaction failed: {:?}", e)))?;
        
        self.flush()?;
        Ok(())
    }

    /// Get transaction count
    pub fn transaction_count(&self) -> Result<usize, StorageError> {
        let tree = self.tree(TreeName::Transactions);
        Ok(tree.iter().count())
    }

    /// Flush database to disk (atomic write)
    pub fn flush(&self) -> Result<(), StorageError> {
        self.db.flush()?;
        Ok(())
    }

    /// Check if migration from JSON is needed
    pub fn needs_migration<P: AsRef<Path>>(&self, json_path: P) -> bool {
        // Check if Sled DB is empty
        let tx_count = self.transaction_count().unwrap_or(0);
        let balance_count = self.get_all_balances().map(|b| b.len()).unwrap_or(0);

        if tx_count > 0 || balance_count > 0 {
            // DB has data, no migration needed
            return false;
        }

        // Check if JSON files exist
        let json_path = json_path.as_ref();
        json_path.exists()
    }

    /// Migrate from JSON storage to Sled
    pub fn migrate_from_json<P: AsRef<Path>>(&self, json_path: P) -> Result<(), StorageError> {
        tracing::info!("🔄 Starting migration from JSON to Sled...");

        // Load JSON DAG
        let dag_path = json_path.as_ref().join("dag.json");
        if dag_path.exists() {
            use crate::json_storage::{DagStore, load_dag_from_json};
            let store: DagStore = tokio::runtime::Runtime::new()
                .map_err(|e| StorageError::DatabaseError(format!("Failed to create runtime: {}", e)))?
                .block_on(load_dag_from_json(&dag_path))?;

            tracing::info!("  Migrating {} transactions from JSON...", store.transactions.len());

            // Migrate transactions
            for stored_tx in store.transactions {
                let signature = if let Some(sig) = stored_tx.signature {
                    hex::decode(&sig).unwrap_or_else(|_| vec![0u8; 64])
                } else {
                    vec![0u8; 64]
                };
                let public_key = if let Some(pk) = stored_tx.public_key {
                    hex::decode(&pk).unwrap_or_else(|_| vec![0u8; 32])
                } else {
                    vec![0u8; 32]
                };

                let parent0_bytes = hex::decode(&stored_tx.parents[0]).unwrap_or_else(|_| vec![0u8; 32]);
                let parent1_bytes = hex::decode(&stored_tx.parents[1]).unwrap_or_else(|_| vec![0u8; 32]);
                let sender_bytes = hex::decode(&stored_tx.sender).unwrap_or_else(|_| vec![0u8; 32]);
                let receiver_bytes = hex::decode(&stored_tx.receiver).unwrap_or_else(|_| vec![0u8; 32]);

                let parent0: TransactionId = parent0_bytes.try_into().unwrap_or([0u8; 32]);
                let parent1: TransactionId = parent1_bytes.try_into().unwrap_or([0u8; 32]);
                let sender: Address = sender_bytes.try_into().unwrap_or([0u8; 32]);
                let receiver: Address = receiver_bytes.try_into().unwrap_or([0u8; 32]);

                let mut tx = Transaction::new(
                    [parent0, parent1],
                    sender,
                    receiver,
                    stored_tx.amount,
                    stored_tx.fee,
                    stored_tx.timestamp,
                    stored_tx.nonce,
                    stored_tx.account_nonce, // 0 for pre-nonce-era historical transactions (safe - these are already processed)
                    signature,
                    public_key,
                );
                tx.weight = stored_tx.weight;

                self.put_transaction(&tx)?;
            }
        }

        // Load JSON Ledger
        let ledger_path = json_path.as_ref().join("ledger.json");
        if ledger_path.exists() {
            
            let ledger_json: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&ledger_path)
                    .map_err(|e| StorageError::DatabaseError(format!("Failed to read ledger: {}", e)))?
            ).map_err(|e| StorageError::DatabaseError(format!("Failed to parse ledger: {}", e)))?;

            let balances_obj = ledger_json.get("balances")
                .and_then(|v| v.as_object())
                .ok_or_else(|| StorageError::DatabaseError("Invalid ledger format".to_string()))?;

            tracing::info!("  Migrating {} balances from JSON...", balances_obj.len());

            // Migrate balances
            for (addr_hex, balance) in balances_obj {
                let balance = balance.as_u64()
                    .ok_or_else(|| StorageError::DatabaseError("Invalid balance value".to_string()))?;
                let addr_bytes = hex::decode(addr_hex)
                    .map_err(|e| StorageError::DatabaseError(format!("Failed to decode address: {}", e)))?;
                let address: Address = addr_bytes.as_slice().try_into()
                    .map_err(|_| StorageError::DatabaseError("Invalid address length".to_string()))?;
                self.put_balance(address, balance)?;
            }
        }

        // Mark migration as complete
        self.put_metadata("migration_complete", b"true")?;

        tracing::info!("✅ Migration from JSON to Sled completed successfully");
        self.flush()?;
        Ok(())
    }

    // ==================== STAKING FUNCTIONS ====================

    /// Stake tokens - move funds from main balance to staking
    pub fn stake_tokens(&self, address: Address, amount: u64) -> Result<(), StorageError> {
        // Check if address has enough balance
        let current_balance = self.get_balance(address)?;
        if current_balance < amount {
            return Err(StorageError::DatabaseError("Insufficient balance for staking".to_string()));
        }

        // Deduct from main balance
        let new_balance = current_balance - amount;
        self.put_balance(address, new_balance)?;

        // Get or create staking position
        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        let mut position = self.get_staking_position(address).unwrap_or(StakingPosition {
            address,
            staked_amount: 0,
            start_timestamp: current_timestamp,
            rewards_earned: 0,
            last_reward_timestamp: current_timestamp,
        });

        // Update position
        position.staked_amount += amount;
        position.last_reward_timestamp = current_timestamp;

        // Save position
        self.put_staking_position(&position)?;

        Ok(())
    }

    /// Unstake tokens - move funds from staking back to main balance
    pub fn unstake_tokens(&self, address: Address) -> Result<u64, StorageError> {
        let mut position = self.get_staking_position(address)
            .ok_or(StorageError::DatabaseError("No staking position found".to_string()))?;

        // Calculate final rewards
        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        let additional_rewards = self.calculate_staking_reward_internal(&position, current_timestamp);
        position.rewards_earned += additional_rewards;

        // Total to return = staked amount + rewards
        let total_return = position.staked_amount + position.rewards_earned;

        // Add to main balance
        let current_balance = self.get_balance(address)?;
        self.put_balance(address, current_balance + total_return)?;

        // Remove staking position
        self.remove_staking_position(address)?;

        Ok(total_return)
    }

    /// Get staking position for an address
    pub fn get_staking_position(&self, address: Address) -> Option<StakingPosition> {
        let tree = self.tree(TreeName::Staking);
        let value = tree.get(address).ok()??;
        let position: StakingPosition = bincode::deserialize(&value).ok()?;
        Some(position)
    }

    /// Put staking position
    fn put_staking_position(&self, position: &StakingPosition) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Staking);
        let value = bincode::serialize(position)?;
        tree.insert(position.address, value)?;
        Ok(())
    }

    /// Remove staking position
    fn remove_staking_position(&self, address: Address) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Staking);
        tree.remove(address)?;
        Ok(())
    }

    /// Calculate staking reward for an address (5% annual, calculated per block)
    pub fn calculate_staking_reward(&self, address: Address) -> Result<u64, StorageError> {
        let position = self.get_staking_position(address)
            .ok_or(StorageError::DatabaseError("No staking position found".to_string()))?;

        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        let total_rewards = self.calculate_staking_reward_internal(&position, current_timestamp);
        Ok(total_rewards)
    }

    /// Internal reward calculation
    fn calculate_staking_reward_internal(&self, position: &StakingPosition, current_timestamp: u64) -> u64 {
        // 5% annual reward = 0.05 per year
        // Calculate time elapsed in seconds
        let time_elapsed = current_timestamp.saturating_sub(position.last_reward_timestamp);
        
        // Convert to years (assuming 365.25 days per year)
        let years_elapsed = time_elapsed as f64 / (365.25 * 24.0 * 3600.0);
        
        // Calculate reward: staked_amount * 0.05 * years_elapsed
        let reward = position.staked_amount as f64 * 0.05 * years_elapsed;
        
        reward as u64
    }

    /// Get total staked amount for an address
    pub fn get_staked_amount(&self, address: Address) -> Result<u64, StorageError> {
        let position = self.get_staking_position(address)
            .ok_or(StorageError::DatabaseError("No staking position found".to_string()))?;
        Ok(position.staked_amount)
    }

    /// Check if an address has staked tokens
    pub fn has_staked_tokens(&self, address: Address) -> bool {
        self.get_staking_position(address).is_some()
    }

    // ==================== NONCE FUNCTIONS ====================

    /// Store account nonce for replay protection
    pub fn put_nonce(&self, address: Address, nonce: u64) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Nonces);
        let value = nonce.to_le_bytes().to_vec();
        tree.insert(address, value)?;
        Ok(())
    }

    /// Put an orphan transaction (transaction waiting for parents)
    /// Economic policy: orphans persist to survive node restart
    pub fn put_orphan(&self, tx_id: TransactionId, tx: &Transaction) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Orphans);
        let value = bincode::serialize(tx)?;
        tree.insert(tx_id, value)?;
        Ok(())
    }

    /// Get an orphan transaction by ID
    pub fn get_orphan(&self, tx_id: TransactionId) -> Result<Option<Transaction>, StorageError> {
        let tree = self.tree(TreeName::Orphans);
        match tree.get(tx_id) {
            Ok(Some(value)) => {
                let tx = bincode::deserialize(&value)?;
                Ok(Some(tx))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get all orphan transactions
    pub fn get_all_orphans(&self) -> Result<Vec<Transaction>, StorageError> {
        let tree = self.tree(TreeName::Orphans);
        let mut orphans = Vec::new();
        for result in tree.iter() {
            let (_, value) = result?;
            let tx = bincode::deserialize(&value)?;
            orphans.push(tx);
        }
        Ok(orphans)
    }

    /// Remove an orphan transaction
    pub fn remove_orphan(&self, tx_id: TransactionId) -> Result<(), StorageError> {
        let tree = self.tree(TreeName::Orphans);
        tree.remove(tx_id)?;
        Ok(())
    }

    /// Get account nonce for an address
    pub fn get_nonce(&self, address: Address) -> Result<u64, StorageError> {
        let tree = self.tree(TreeName::Nonces);
        let value = tree.get(address)?.ok_or(StorageError::KeyNotFound)?;
        let nonce = u64::from_le_bytes(value.as_ref().try_into()
            .map_err(|_| StorageError::DatabaseError("Invalid nonce value length".to_string()))?);
        Ok(nonce)
    }

    /// Get all nonces
    pub fn get_all_nonces(&self) -> Result<HashMap<Address, u64>, StorageError> {
        let tree = self.tree(TreeName::Nonces);
        let mut nonces = HashMap::new();

        for item in tree.iter() {
            let (key, value) = item.map_err(|e| StorageError::DatabaseError(e.to_string()))?;
            let address: Address = key.as_ref().try_into()
                .map_err(|_| StorageError::DatabaseError("Invalid address length".to_string()))?;
            let nonce = u64::from_le_bytes(value.as_ref().try_into()
                .map_err(|_| StorageError::DatabaseError("Invalid nonce value length".to_string()))?);
            nonces.insert(address, nonce);
        }

        Ok(nonces)
    }
}

/// Batch operation for atomic writes
pub enum BatchOperation {
    PutTransaction(Transaction),
    PutBalance(Address, u64),
    DeleteTransaction(TransactionId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_storage_open() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        assert!(true);
    }

    #[test]
    fn test_put_get_transaction() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        storage.put_transaction(&tx).unwrap();
        let retrieved = storage.get_transaction(tx.id).unwrap();

        assert_eq!(retrieved.id, tx.id);
        assert_eq!(retrieved.sender, tx.sender);
        assert_eq!(retrieved.receiver, tx.receiver);
    }

    #[test]
    fn test_transaction_exists() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            0,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        assert!(!storage.transaction_exists(tx.id).unwrap());

        storage.put_transaction(&tx).unwrap();
        assert!(storage.transaction_exists(tx.id).unwrap());
    }

    #[test]
    fn test_delete_transaction() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            0,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        storage.put_transaction(&tx).unwrap();
        assert!(storage.transaction_exists(tx.id).unwrap());

        storage.delete_transaction(tx.id).unwrap();
        assert!(!storage.transaction_exists(tx.id).unwrap());
    }

    #[test]
    fn test_get_all_transactions() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let tx1 = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            0,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        let tx2 = Transaction::new(
            [[0u8; 32]; 2],
            [3u8; 32],
            [4u8; 32],
            200,
            10,
            0,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        storage.put_transaction(&tx1).unwrap();
        storage.put_transaction(&tx2).unwrap();

        let transactions = storage.get_all_transactions().unwrap();
        assert_eq!(transactions.len(), 2);
    }

    #[test]
    fn test_put_get_balance() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let address = [1u8; 32];
        let balance = 1000;

        storage.put_balance(address, balance).unwrap();
        let retrieved = storage.get_balance(address).unwrap();

        assert_eq!(retrieved, balance);
    }

    #[test]
    fn test_get_all_balances() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let addr1 = [1u8; 32];
        let addr2 = [2u8; 32];

        storage.put_balance(addr1, 1000).unwrap();
        storage.put_balance(addr2, 2000).unwrap();

        let balances = storage.get_all_balances().unwrap();
        assert_eq!(balances.len(), 2);
        assert_eq!(balances.get(&addr1), Some(&1000));
        assert_eq!(balances.get(&addr2), Some(&2000));
    }

    #[test]
    fn test_put_get_metadata() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let key = "test_key";
        let value = b"test_value";

        storage.put_metadata(key, value).unwrap();
        let retrieved = storage.get_metadata(key).unwrap();

        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_batch_write() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            0,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        let address = [3u8; 32];

        let operations = vec![
            BatchOperation::PutTransaction(tx.clone()),
            BatchOperation::PutBalance(address, 500),
        ];

        storage.batch_write(operations).unwrap();

        assert!(storage.transaction_exists(tx.id).unwrap());
        assert_eq!(storage.get_balance(address).unwrap(), 500);
    }

    #[test]
    fn test_transaction_count() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();

        assert_eq!(storage.transaction_count().unwrap(), 0);

        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            0,
            0,
            1,
            vec![0u8; 64],
            vec![0u8; 32],
        );

        storage.put_transaction(&tx).unwrap();
        assert_eq!(storage.transaction_count().unwrap(), 1);
    }
}
