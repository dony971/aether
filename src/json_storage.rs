//! # JSON Storage Module
//!
//! Simple JSON-based persistence for DAG state on Windows.
//! Alternative to RocksDB for Windows compatibility.

use crate::transaction::Transaction;
use crate::parent_selection::DAG;
use serde::{Deserialize, Serialize};
use tokio::fs;
use std::path::PathBuf;

/// JSON-serializable DAG state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagStore {
    pub transactions: Vec<StoredTransaction>,
    pub children: Vec<StoredChild>,
}

/// Stored transaction for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTransaction {
    pub id: String,
    pub parents: [String; 2],
    pub sender: String,
    pub receiver: String,
    pub amount: u64,
    pub fee: u64,
    pub timestamp: u64,
    pub nonce: u64,
    pub account_nonce: u64,
    pub weight: f64,
    pub signature: Option<String>,
    pub public_key: Option<String>,
}

/// Stored child reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChild {
    pub parent: String,
    pub child: String,
}

impl From<&Transaction> for StoredTransaction {
    fn from(tx: &Transaction) -> Self {
        StoredTransaction {
            id: hex::encode(tx.id),
            parents: [
                hex::encode(tx.parents[0]),
                hex::encode(tx.parents[1]),
            ],
            sender: hex::encode(tx.sender),
            receiver: hex::encode(tx.receiver),
            amount: tx.amount,
            fee: tx.fee,
            timestamp: tx.timestamp,
            nonce: tx.nonce,
            account_nonce: tx.account_nonce,
            weight: tx.weight,
            signature: Some(hex::encode(tx.signature.clone())),
            public_key: Some(hex::encode(tx.public_key.clone())),
        }
    }
}

/// Save DAG state to JSON file atomically
pub async fn save_dag_to_json(dag: &DAG, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let transactions: Vec<StoredTransaction> = dag.transactions()
        .values()
        .map(StoredTransaction::from)
        .collect();

    let children: Vec<StoredChild> = dag.children()
        .iter()
        .flat_map(|(parent, children)| {
            children.iter().map(move |child| StoredChild {
                parent: hex::encode(parent),
                child: hex::encode(child),
            })
        })
        .collect();

    let store = DagStore {
        transactions,
        children,
    };

    let json = serde_json::to_string_pretty(&store)?;

    // Write to temporary file first for atomicity
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, json).await?;

    // Atomic rename to replace the original file
    fs::rename(&tmp_path, path).await?;

    tracing::debug!("💾 DAG saved atomically to {:?}", path);
    Ok(())
}

/// Load DAG state from JSON file
pub async fn load_dag_from_json(path: &PathBuf) -> Result<DagStore, Box<dyn std::error::Error>> {
    let json = fs::read_to_string(path).await?;
    let store: DagStore = serde_json::from_str(&json)?;
    Ok(store)
}

/// Ensure data directory exists
pub async fn ensure_data_dir(data_dir: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if !data_dir.exists() {
        fs::create_dir_all(data_dir).await?;
    }
    Ok(())
}
