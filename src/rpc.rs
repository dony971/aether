//! # RPC Module
//!
//! Implements JSON-RPC server for external communication using jsonrpsee with Axum integration.

use crate::transaction::{Transaction, TransactionId, Address};
use crate::consensus::VQVConsensus;
use crate::parent_selection::DAG;
use crate::ledger::Ledger;
use crate::transaction_processor::TransactionProcessor;
use crate::SyncEvent;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore, mpsc};
use axum::{
    routing::{get, post},
    Router,
    response::Html,
    extract::State,
    Json,
};
use tower_http::cors::CorsLayer;

/// Custom RPC error
#[derive(Debug)]
pub struct RpcError(String);

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RpcError {}

/// Balance response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub address: Address,
    pub balance: u64,
    pub mining_rewards: u64,
}

/// Transaction response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionResponse {
    pub tx_id: TransactionId,
    pub status: String,
    pub message: String,
}

/// DAG stats response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagStatsResponse {
    pub current_tps: f64,
    pub total_transactions: u64,
    pub tip_count: usize,
    pub epoch: u64,
    pub connected_peers: u32,
}

/// Hashrate response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashrateResponse {
    pub hashrate: String,
    pub difficulty: u64,
}

/// Global status - canonical status all nodes converge on
/// Economic policy: quorum-based weighted convergence for safety + liveness
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GlobalStatus {
    /// Transaction unknown to querying node (may exist elsewhere)
    /// Economic impact: unknown - not rejected, just not seen
    Unknown,
    
    /// Transaction pending (in mempool or orphan, not yet in DAG)
    /// Economic impact: not yet accepted, may be resolved
    Pending,
    
    /// Transaction in DAG but insufficient references/weight
    /// Economic impact: visible but not stable, may be reorganized
    Unconfirmed,
    
    /// Transaction in DAG with sufficient references (≥3)
    /// Economic impact: stable, reorganization unlikely but possible
    Confirmed,
    
    /// Transaction economically stable (weight ≥5.0)
    /// Economic impact: reorganization extremely unlikely, practically final
    Stable,
    
    /// Transaction finalized by VQV consensus votes
    /// Economic impact: irreversible, guaranteed by protocol
    Finalized,
}

/// Node status report for quorum-based reconciliation
#[derive(Debug, Clone, Copy)]
pub struct NodeStatusReport {
    pub weight: f64,
    pub local_status: LocalStatus,
    pub consensus_status: ConsensusStatus,
}

/// Quorum-based global status resolver
/// Economic policy: safety via quorum, liveness via majority fallback
pub struct GlobalStatusResolver {
    /// Minimum weight required for quorum (default 2/3)
    quorum_threshold: f64,
    /// Minimum weight required for majority fallback (default 1/2)
    majority_threshold: f64,
}

impl GlobalStatusResolver {
    /// Create new resolver with default thresholds
    pub fn new() -> Self {
        Self {
            quorum_threshold: 0.67, // 2/3 for safety
            majority_threshold: 0.50, // 1/2 for liveness
        }
    }
    
    /// Create resolver with custom thresholds
    pub fn with_thresholds(quorum: f64, majority: f64) -> Self {
        Self {
            quorum_threshold: quorum,
            majority_threshold: majority,
        }
    }
    
    /// Reconcile multiple node statuses into single global status
    /// Economic policy: quorum-based weighted convergence
    pub fn reconcile_quorum(&self, reports: &[NodeStatusReport]) -> GlobalStatus {
        if reports.is_empty() {
            return GlobalStatus::Unknown;
        }
        
        let total_weight: f64 = reports.iter().map(|r| r.weight).sum();
        if total_weight == 0.0 {
            return GlobalStatus::Unknown;
        }
        
        // Calculate weight for each status level (ascending)
        let mut weight_unknown = 0.0;
        let mut weight_pending = 0.0;
        let mut weight_unconfirmed = 0.0;
        let mut weight_confirmed = 0.0;
        let mut weight_stable = 0.0;
        let mut weight_finalized = 0.0;
        
        for report in reports {
            let global = GlobalStatusResolver::reconcile_single(report.local_status, report.consensus_status);
            match global {
                GlobalStatus::Unknown => weight_unknown += report.weight,
                GlobalStatus::Pending => weight_pending += report.weight,
                GlobalStatus::Unconfirmed => weight_unconfirmed += report.weight,
                GlobalStatus::Confirmed => weight_confirmed += report.weight,
                GlobalStatus::Stable => weight_stable += report.weight,
                GlobalStatus::Finalized => weight_finalized += report.weight,
            }
        }
        
        // Normalize weights
        let w_unknown = weight_unknown / total_weight;
        let w_pending = weight_pending / total_weight;
        let w_unconfirmed = weight_unconfirmed / total_weight;
        let w_confirmed = weight_confirmed / total_weight;
        let w_stable = weight_stable / total_weight;
        let w_finalized = weight_finalized / total_weight;
        
        // Check for quorum at each level (highest first)
        // Finalized requires explicit VQV votes - not just weight
        if w_finalized >= self.quorum_threshold {
            GlobalStatus::Finalized
        } else if w_stable >= self.quorum_threshold {
            GlobalStatus::Stable
        } else if w_confirmed >= self.quorum_threshold {
            GlobalStatus::Confirmed
        } else if w_unconfirmed >= self.quorum_threshold {
            GlobalStatus::Unconfirmed
        } else if w_pending >= self.quorum_threshold {
            GlobalStatus::Pending
        } else if w_unknown >= self.quorum_threshold {
            GlobalStatus::Unknown
        } else {
            // No quorum - use majority fallback for liveness
            if w_finalized >= self.majority_threshold {
                GlobalStatus::Finalized
            } else if w_stable >= self.majority_threshold {
                GlobalStatus::Stable
            } else if w_confirmed >= self.majority_threshold {
                GlobalStatus::Confirmed
            } else if w_unconfirmed >= self.majority_threshold {
                GlobalStatus::Unconfirmed
            } else if w_pending >= self.majority_threshold {
                GlobalStatus::Pending
            } else {
                // Default to unknown if no majority
                GlobalStatus::Unknown
            }
        }
    }
    
    /// Reconcile single node's local and consensus status
    /// Economic policy: conservative convergence for single node
    pub fn reconcile_single(local: LocalStatus, consensus: ConsensusStatus) -> GlobalStatus {
        match local {
            LocalStatus::Unknown => GlobalStatus::Unknown,
            LocalStatus::Orphan => GlobalStatus::Pending,
            LocalStatus::InMempool => GlobalStatus::Pending,
            LocalStatus::InLocalDag => {
                match consensus {
                    ConsensusStatus::Unconfirmed => GlobalStatus::Unconfirmed,
                    ConsensusStatus::Confirmed => GlobalStatus::Confirmed,
                    ConsensusStatus::Stable => GlobalStatus::Stable,
                    ConsensusStatus::Finalized => GlobalStatus::Finalized,
                }
            }
        }
    }
}

impl Default for GlobalStatusResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalStatus {
    /// Minimum number of references for "confirmed" status
    pub const MIN_CONFIRMATIONS: usize = 3;
    
    /// Minimum cumulative weight for "stable" status
    pub const STABILITY_THRESHOLD: f64 = 5.0;
    
    /// Check if status is considered "final" for practical purposes
    pub fn is_practically_final(&self) -> bool {
        matches!(self, Self::Stable | Self::Finalized)
    }
    
    /// Convert to string for RPC response
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Pending => "pending",
            Self::Unconfirmed => "unconfirmed",
            Self::Confirmed => "confirmed",
            Self::Stable => "stable",
            Self::Finalized => "finalized",
        }
    }
    
    /// Reconcile local and consensus status into global status (single node)
    /// Economic policy: conservative convergence - choose minimum certainty
    /// For multi-node reconciliation, use GlobalStatusResolver::reconcile_quorum
    pub fn reconcile(local: LocalStatus, consensus: ConsensusStatus) -> Self {
        GlobalStatusResolver::reconcile_single(local, consensus)
    }
}

impl std::fmt::Display for GlobalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Local status of a transaction from this node's perspective
/// Economic policy: reflects what this node knows, not global consensus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalStatus {
    /// Transaction unknown to this node (may exist elsewhere)
    /// Economic impact: unknown - not rejected, just not seen
    Unknown,
    
    /// Transaction is waiting for missing parents (orphan)
    /// Economic impact: not yet accepted, may be resolved when parents arrive
    Orphan,
    
    /// Transaction accepted locally (in mempool) but not yet in DAG
    /// Economic impact: ledger committed, but may be reorganized
    InMempool,
    
    /// Transaction is in this node's DAG
    /// Economic impact: visible locally, consensus status separate
    InLocalDag,
}

impl LocalStatus {
    /// Convert to string for RPC response
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Orphan => "orphan",
            Self::InMempool => "in_mempool",
            Self::InLocalDag => "in_local_dag",
        }
    }
}

impl std::fmt::Display for LocalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Consensus status of a transaction from network perspective
/// Economic policy: reflects global stability, not local knowledge
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsensusStatus {
    /// Not yet confirmed by network (insufficient references/weight)
    /// Economic impact: may be reorganized
    Unconfirmed,
    
    /// Confirmed by sufficient references
    /// Economic policy: confirmed = at least MIN_CONFIRMATIONS references
    /// Economic impact: stable, reorganization unlikely but possible
    Confirmed,
    
    /// Economically stable (high weight/references)
    /// Economic policy: economically_stable = weight above STABILITY_THRESHOLD
    /// Economic impact: reorganization extremely unlikely, practically final
    Stable,
    
    /// Finalized by consensus mechanism
    /// Economic impact: irreversible, guaranteed by protocol (VQV votes)
    Finalized,
}

impl ConsensusStatus {
    /// Minimum number of references for "confirmed" status
    pub const MIN_CONFIRMATIONS: usize = 3;
    
    /// Minimum cumulative weight for "stable" status
    pub const STABILITY_THRESHOLD: f64 = 5.0;
    
    /// Check if status is considered "final" for practical purposes
    pub fn is_practically_final(&self) -> bool {
        matches!(self, Self::Stable | Self::Finalized)
    }
    
    /// Convert to string for RPC response
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unconfirmed => "unconfirmed",
            Self::Confirmed => "confirmed",
            Self::Stable => "stable",
            Self::Finalized => "finalized",
        }
    }
}

impl std::fmt::Display for ConsensusStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Combined transaction status for distributed environment
/// Economic policy: separates local knowledge from global consensus with resolution layer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionStatus {
    pub local_status: LocalStatus,
    pub consensus_status: ConsensusStatus,
    pub global_status: GlobalStatus,
    pub practically_final: bool,
}

impl TransactionStatus {
    /// Create new transaction status with global reconciliation
    pub fn new(local_status: LocalStatus, consensus_status: ConsensusStatus) -> Self {
        let global_status = GlobalStatus::reconcile(local_status, consensus_status);
        let practically_final = global_status.is_practically_final();
        Self {
            local_status,
            consensus_status,
            global_status,
            practically_final,
        }
    }
    
    /// Convert to string for RPC response (combined status)
    pub fn as_str(&self) -> String {
        format!("{}:{}:{}", self.local_status.as_str(), self.consensus_status.as_str(), self.global_status.as_str())
    }
}

/// Transaction status response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionStatusResponse {
    pub tx_id: TransactionId,
    pub local_status: String,
    pub consensus_status: String,
    pub global_status: String,
    pub confirmed: bool,
    pub practically_final: bool,
    pub block_height: Option<u64>,
    pub timestamp: Option<u64>,
    pub reference_count: usize,
    pub weight: f64,
}

/// Transaction history item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionHistoryItem {
    pub hash: String,
    pub sender: String,
    pub receiver: String,
    pub amount: u64,
    pub timestamp: u64,
    pub is_incoming: bool,
}

/// Transaction history response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionHistoryResponse {
    pub transactions: Vec<TransactionHistoryItem>,
    pub total_count: usize,
}

/// Staking response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingResponse {
    pub address: Address,
    pub staked_amount: u64,
    pub rewards_earned: u64,
    pub success: bool,
}

/// Staking info response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingInfoResponse {
    pub staked_amount: u64,
    pub rewards_earned: u64,
    pub has_staked: bool,
}

/// Account nonce response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountNonceResponse {
    pub address: Address,
    pub current_nonce: u64,
    pub next_nonce: u64,
}

/// Transaction finality states (practical finality semantics)
/// Economic policy: transactions progress through these states as they gain consensus confidence
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionFinality {
    /// Accepted locally: in mempool and DAG, but not yet confirmed by network
    /// Economic impact: ledger committed, fees burned, but may be reorganized
    Accepted,
    /// Visible in DAG: confirmed by network, but not yet final
    /// Economic impact: stable, but theoretical reorganization possible
    Confirmed,
    /// Economically stable: sufficient tips/confirmations for practical finality
    /// Economic impact: reorganization extremely unlikely, can be considered final for most use cases
    EconomicallyStable,
    /// Finalized: confirmed by consensus mechanism (VQV votes)
    /// Economic impact: irreversible, guaranteed by protocol
    Finalized,
}

impl TransactionFinality {
    /// Check if transaction is considered "final" for practical purposes
    /// Economic policy: EconomicallyStable and Finalized are both practically final
    pub fn is_practically_final(&self) -> bool {
        matches!(self, Self::EconomicallyStable | Self::Finalized)
    }
}

/// Mempool for transaction queuing with economic priority
/// Economic policy: transactions are prioritized by fee rate (fee per unit of work)
pub struct Mempool {
    queue: VecDeque<Transaction>,
    max_size: usize,
    semaphore: Arc<Semaphore>,
    /// Minimum fee required for a transaction to be accepted
    min_fee: u64,
}

impl Mempool {
    /// Create new mempool with max size and minimum fee
    pub fn new(max_size: usize, max_concurrent: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(max_size),
            max_size,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            min_fee: 1, // Minimum fee of 1 unit (anti-spam)
        }
    }
    
    /// Set minimum fee
    pub fn set_min_fee(&mut self, min_fee: u64) {
        self.min_fee = min_fee;
    }
    
    /// Get minimum fee
    pub fn min_fee(&self) -> u64 {
        self.min_fee
    }
    
    /// Add transaction to mempool with economic validation
    /// Economic policy: transaction must meet minimum fee requirement
    /// When mempool is full, lower-fee transactions may be evicted to make room for higher-fee ones
    /// Add transaction to mempool (INTERNAL USE ONLY)
    /// 🔒 ZERO TRUST: This method is private. Use TransactionProcessor for all mempool operations.
    pub(crate) async fn add_internal(&mut self, tx: Transaction) -> Result<(), RpcError> {
        // Economic validation: check minimum fee
        if tx.fee < self.min_fee {
            return Err(RpcError(format!("Insufficient fee: {} < minimum {}", tx.fee, self.min_fee)));
        }
        
        // If mempool is full, try to evict lower-fee transactions
        if self.queue.len() >= self.max_size {
            // Check if this transaction has higher fee than the lowest in mempool
            let min_fee_in_pool = self.queue.iter().map(|t| t.fee).min().unwrap_or(0);
            
            if tx.fee > min_fee_in_pool {
                // Evict the lowest-fee transaction to make room
                if let Some(pos) = self.queue.iter().position(|t| t.fee == min_fee_in_pool) {
                    self.queue.remove(pos);
                    tracing::info!("🔄 Evicted low-fee transaction (fee: {}) to make room for higher-fee (fee: {})", min_fee_in_pool, tx.fee);
                }
            } else {
                return Err(RpcError("Mempool full (consider higher fee for priority)".to_string()));
            }
        }

        self.queue.push_back(tx);
        Ok(())
    }
    
    /// Get transaction semaphore for rate limiting
    pub fn semaphore(&self) -> Arc<Semaphore> {
        self.semaphore.clone()
    }
    
    /// Get queue size
    pub fn size(&self) -> usize {
        self.queue.len()
    }
    
    /// Get max size
    pub fn max_size(&self) -> usize {
        self.max_size
    }
    
    /// Pop transaction from mempool (FIFO)
    pub fn pop_front(&mut self) -> Option<Transaction> {
        self.queue.pop_front()
    }

    /// Remove transaction by ID (for rollback)
    pub fn remove_transaction(&mut self, tx_id: &TransactionId) {
        self.queue.retain(|tx| &tx.id != tx_id);
    }
    
    /// Get all transaction IDs in mempool (for testing)
    pub fn get_transaction_ids(&self) -> Vec<TransactionId> {
        self.queue.iter().map(|tx| tx.id).collect()
    }
}

/// Recent transactions response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentTransactionsResponse {
    pub transactions: Vec<TransactionInfo>,
    pub total_count: u64,
}

/// Transaction info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInfo {
    pub tx_id: TransactionId,
    pub sender: Address,
    pub receiver: Address,
    pub amount: u64,
    pub fee: u64,
    pub parents: [TransactionId; 2],
    pub timestamp: u64,
    pub status: String,
}

/// DAG graph response for visualization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagGraphResponse {
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    pub total_transactions: usize,
}

/// DAG node for visualization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    pub tx_id: TransactionId,
    pub sender: Address,
    pub receiver: Address,
    pub amount: u64,
    pub timestamp: u64,
    pub weight: f64,
}

/// DAG edge for visualization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: TransactionId,
    pub to: TransactionId,
}

/// Tips response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TipsResponse {
    pub tips: Vec<TransactionId>,
    pub count: usize,
}

/// DAG snapshot response for explorer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagSnapshotResponse {
    pub transactions: Vec<TransactionSnapshot>,
    pub count: usize,
}

/// Transaction snapshot with weight and signature validity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionSnapshot {
    pub hash: String,
    pub parents: Vec<String>,
    pub cumulative_weight: f64,
    pub signature_valid: bool,
    pub sender: String,
    pub receiver: String,
    pub amount: u64,
    pub fee: u64,
    pub nonce: u64,
    pub timestamp: u64,
}

/// Mining status response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningStatusResponse {
    pub is_mining: bool,
    pub hashrate: String,
}

/// Faucet response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaucetResponse {
    pub success: bool,
    pub amount: u64,
    pub message: String,
}

/// Create account response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAccountResponse {
    pub success: bool,
    pub address: String,
    pub public_key: String,
    pub private_key: String,
    pub message: String,
}

/// RPC server implementation
pub struct AetherRpcImpl {
    consensus: Arc<RwLock<VQVConsensus>>,
    dag: Arc<RwLock<DAG>>,
    ledger: Arc<RwLock<Ledger>>,
    storage: Arc<RwLock<crate::storage::Storage>>,
    ledger_path: std::path::PathBuf,
    mempool: Arc<RwLock<Mempool>>,
    p2p_network: Arc<crate::p2p::P2PNetwork>,
    save_tx: mpsc::Sender<SyncEvent>,
    mining_enabled: Arc<RwLock<bool>>,
    miner_address: Option<Address>,
    orphans: Arc<RwLock<std::collections::HashMap<[u8; 32], Transaction>>>,
}

impl AetherRpcImpl {
    /// Create new RPC implementation
    pub fn new(
        consensus: Arc<RwLock<VQVConsensus>>,
        dag: Arc<RwLock<DAG>>,
        ledger: Arc<RwLock<Ledger>>,
        storage: Arc<RwLock<crate::storage::Storage>>,
        ledger_path: std::path::PathBuf,
        mempool: Arc<RwLock<Mempool>>,
        p2p_network: Arc<crate::p2p::P2PNetwork>,
        save_tx: mpsc::Sender<SyncEvent>,
        mining_enabled: Arc<RwLock<bool>>,
        miner_address: Option<Address>,
        orphans: Arc<RwLock<std::collections::HashMap<[u8; 32], Transaction>>>,
    ) -> Self {
        Self {
            consensus,
            dag,
            ledger,
            storage,
            ledger_path,
            mempool,
            p2p_network,
            save_tx,
            mining_enabled,
            miner_address,
            orphans,
        }
    }

    /// Get balance for an address
    pub async fn get_balance(&self, address: Address) -> Result<BalanceResponse, RpcError> {
        let ledger = self.ledger.read().await;

        let balance = ledger.get_balance(&address);
        Ok(BalanceResponse {
            address,
            balance,
            mining_rewards: 0,
        })
    }

    /// Send a transaction
    pub async fn send_transaction(&self, params: serde_json::Value) -> Result<TransactionResponse, RpcError> {
        tracing::error!("RAW RPC PARAMS RECEIVED: {:?}", params);

        // Parse params manually - expect array with single string
        let tx_data: String = match params {
            serde_json::Value::Array(arr) if arr.len() == 1 => {
                match &arr[0] {
                    serde_json::Value::String(s) => s.clone(),
                    _ => {
                        tracing::error!("❌ Expected string in params array, got: {:?}", arr[0]);
                        return Err(RpcError("Invalid params: expected string in array".to_string()));
                    }
                }
            }
            serde_json::Value::String(s) => s.clone(),
            _ => {
                tracing::error!("❌ Expected array or string, got: {:?}", params);
                return Err(RpcError(format!("Invalid params: expected array with string or string, got {:?}", params)));
            }
        };

        tracing::info!("📨 Received RPC send_transaction request");
        tracing::debug!("Transaction data length: {} bytes", tx_data.len());

        // Log raw hex payload
        tracing::info!("Raw hex: {}", tx_data);
        tracing::debug!("Payload RPC reçu: {}", tx_data);

        // Step 1: Reception - Parse transaction from hex string (SDK sends hex-encoded bincode)
        let tx_bytes = match hex::decode(&tx_data) {
            Ok(bytes) => {
                tracing::debug!("✅ Reception: {} bytes decoded from hex", bytes.len());
                bytes
            }
            Err(e) => {
                tracing::error!("❌ Reception - Erreur hex decode: {}", e);
                tracing::error!("Données reçues: {}", tx_data);
                return Err(RpcError(format!("Invalid hex data: {}", e)));
            }
        };

        // Step 2: Parsing - Deserialize transaction using bincode (same format as GUI)
        let tx: Transaction = match bincode::deserialize::<Transaction>(&tx_bytes) {
            Ok(transaction) => {
                tracing::info!("✅ Parsing: Transaction désérialisée pour {}", hex::encode(transaction.sender));
                transaction
            }
            Err(e) => {
                tracing::error!("❌ Parsing - Erreur deserialize: {}", e);
                tracing::error!("Taille des données: {} bytes", tx_bytes.len());

                // Hex Dump of first 16 bytes for debugging
                let hex_dump = if tx_bytes.len() >= 16 {
                    format!("{}", hex::encode(&tx_bytes[..16]))
                } else {
                    format!("{}", hex::encode(&tx_bytes))
                };
                tracing::error!("HEX DUMP (first 16 bytes): {}", hex_dump);

                return Err(RpcError(format!("Invalid transaction data: {}", e)));
            }
        };

        // Use common validation and processing logic
        self.process_transaction(tx, "RPC").await
    }

    /// Common transaction validation and processing logic (used by both RPC and P2P)
    /// 🔒 ZERO TRUST: Uses TransactionProcessor as single entry point
    /// Economic policy: validation BEFORE any state modification, atomic rollback on failure
    pub async fn process_transaction(&self, tx: Transaction, source: &str) -> Result<TransactionResponse, RpcError> {
        // CONSENSUS ACCEPTANCE RULES:
        // - VALID BUT NOT ACCEPTABLE: Transaction passes basic checks (PoW, signature) but has missing parents -> orphaned
        // - DEFINITIVELY INVALID: Invalid PoW, signature, balance, nonce, duplicate, double spend, sender conflict -> rejected
        // - TEMPORARILY DEFERRED: Mempool full, lock contention -> retry later
        // - ACCEPTED: Passes all validation, committed to ledger and DAG -> pending confirmation

        // Validation logs
        tracing::info!("🔍 Processing transaction [{}] - Sender: {}", source, hex::encode(tx.sender));
        tracing::info!("🔍 Processing transaction [{}] - Receiver: {}", source, hex::encode(tx.receiver));
        tracing::info!("🔍 Processing transaction [{}] - Amount: {}", source, tx.amount);
        tracing::info!("🔍 Processing transaction [{}] - Fee: {}", source, tx.fee);
        tracing::info!("🔍 Processing transaction [{}] - Parents: [{}, {}]", source, hex::encode(tx.parents[0]), hex::encode(tx.parents[1]));
        tracing::info!("🔍 Processing transaction [{}] - PoW Nonce: {}", source, tx.nonce);
        tracing::info!("🔍 Processing transaction [{}] - Account Nonce: {}", source, tx.account_nonce);

        // STEP 1: CHECK FOR MISSING PARENTS (orphan handling - before full validation)
        // This is a special case: valid transactions with missing parents are stored as orphans
        let dag = match self.dag.try_read() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("❌ DAG lock error: {}", e);
                return Err(RpcError("DAG lock error".to_string()));
            }
        };

        let mut missing_parents = Vec::new();
        for (i, parent) in tx.parents.iter().enumerate() {
            let is_genesis = *parent == [0u8; 32];
            if !is_genesis && !dag.transactions().contains_key(parent) {
                tracing::warn!("⚠️ Parent {} missing: {} - requesting via P2P", i, hex::encode(parent));
                missing_parents.push((i, parent.clone()));

                // Request missing parent via P2P
                let parent_hash = parent.to_vec();
                let p2p = self.p2p_network.clone();
                tokio::spawn(async move {
                    tracing::info!("📡 P2P - Requesting missing parent: {}", hex::encode(&parent_hash));
                    p2p.request_transaction(parent_hash).await;
                });
            }
        }

        // VALID BUT NOT ACCEPTABLE: If parents are missing, store as orphan
        if !missing_parents.is_empty() {
            drop(dag);
            
            // Persist orphan to disk (survives restart)
            if let Ok(storage) = crate::storage::Storage::open(self.ledger_path.parent().unwrap_or(std::path::Path::new("data"))) {
                if let Err(e) = storage.put_orphan(tx.id, &tx) {
                    tracing::error!("❌ Failed to persist orphan to storage: {}", e);
                }
            }
            
            // Also keep in memory for fast access
            {
                let mut orphans = self.orphans.write().await;
                orphans.insert(tx.id, tx.clone());
                tracing::info!("📦 Orphan stored: {} (missing {} parent(s)) - persisted to disk", hex::encode(tx.id), missing_parents.len());
            }
            
            return Err(RpcError(format!(
                "Transaction has {} missing parent(s). Stored as orphan and requesting via P2P. Please retry in a few seconds.",
                missing_parents.len()
            )));
        }
        drop(dag);

        // STEP 2: USE TRANSACTION PROCESSOR (ZERO TRUST SINGLE ENTRY POINT)
        // 🔒 All validation and state mutations go through TransactionProcessor
        // 🔒 CRITICAL: Block reward uses consensus state (single source of truth for height)
        let processor = TransactionProcessor::new();
        let mempool = self.mempool.read().await;
        let min_fee = mempool.min_fee();
        drop(mempool);

        let miner_addr = self.miner_address.as_ref();

        // Get consensus state (single source of truth for block height)
        let mut consensus = self.consensus.write().await;
        let consensus_state = consensus.state_mut();
        // Use transaction ID as block ID (in production, this should be the actual block ID from consensus)
        let block_id = Some(tx.id);

        match processor.process(
            tx.clone(),
            &self.dag,
            &self.ledger,
            &self.mempool,
            min_fee,
            miner_addr,
            consensus_state,
            block_id,
        ).await {
            Ok(_) => {
                tracing::info!("✅ Transaction processed successfully via TransactionProcessor");
                Ok(TransactionResponse {
                    tx_id: tx.id,
                    status: "in_mempool".to_string(),
                    message: "Transaction accepted locally (in mempool, not yet in DAG)".to_string(),
                })
            }
            Err(e) => {
                tracing::error!("❌ Transaction processing failed: {}", e);
                Err(RpcError(e.to_string()))
            }
        }
    }

    /// Process orphans - retry transactions that were waiting for parents
    pub async fn process_orphans(&self) {
        let mut orphans_to_process = Vec::new();
        
        // Load orphans from disk on startup
        if let Ok(storage) = crate::storage::Storage::open(self.ledger_path.parent().unwrap_or(std::path::Path::new("data"))) {
            if let Ok(disk_orphans) = storage.get_all_orphans() {
                tracing::info!("📦 Loaded {} orphans from disk", disk_orphans.len());
                for orphan in disk_orphans {
                    let mut orphans = self.orphans.write().await;
                    if !orphans.contains_key(&orphan.id) {
                        orphans.insert(orphan.id, orphan.clone());
                    }
                }
            }
        }
        
        // Check which orphans can now be processed (parents available)
        {
            let orphans = self.orphans.read().await;
            let dag = self.dag.read().await;
            
            for (tx_id, orphan) in orphans.iter() {
                let parent0_ok = orphan.parents[0] == [0u8; 32] || dag.transactions().contains_key(&orphan.parents[0]);
                let parent1_ok = orphan.parents[1] == [0u8; 32] || dag.transactions().contains_key(&orphan.parents[1]);
                
                if parent0_ok && parent1_ok {
                    tracing::info!("🔗 Orphan {} resolved - parents now available", hex::encode(&tx_id[..8]));
                    orphans_to_process.push((*tx_id, orphan.clone()));
                }
            }
        }
        
        // Process resolved orphans
        for (tx_id, orphan) in orphans_to_process {
            tracing::info!("🔄 Re-processing orphan transaction: {}", hex::encode(&tx_id[..8]));
            match self.process_transaction(orphan, "Orphan").await {
                Ok(_) => {
                    tracing::info!("✅ Orphan transaction successfully processed: {}", hex::encode(&tx_id[..8]));
                    // Remove from orphans on success
                    let mut orphans = self.orphans.write().await;
                    orphans.remove(&tx_id);
                    
                    // Also remove from disk
                    if let Ok(storage) = crate::storage::Storage::open(self.ledger_path.parent().unwrap_or(std::path::Path::new("data"))) {
                        let _ = storage.remove_orphan(tx_id);
                    }
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    // Check if error is permanent (replay, double spend, etc.)
                    let is_permanent = error_msg.contains("Duplicate transaction")
                        || error_msg.contains("Double spend")
                        || error_msg.contains("Sender conflict");
                    
                    if is_permanent {
                        tracing::warn!("🗑️ Orphan {} permanently invalid, removing from queue", hex::encode(&tx_id[..8]));
                        let mut orphans = self.orphans.write().await;
                        orphans.remove(&tx_id);
                        
                        // Also remove from disk
                        if let Ok(storage) = crate::storage::Storage::open(self.ledger_path.parent().unwrap_or(std::path::Path::new("data"))) {
                            let _ = storage.remove_orphan(tx_id);
                        }
                    } else {
                        // Temporary error (mempool full, lock error, etc.) - keep in queue
                        tracing::info!("📦 Orphan {} kept in queue (temporary error)", hex::encode(&tx_id[..8]));
                    }
                }
            }
        }
    }

    /// Get DAG statistics
    pub async fn get_dag_stats(&self) -> Result<DagStatsResponse, RpcError> {
        let _dag = self.dag.read().await;
        let consensus = self.consensus.read().await;
        let _mempool = self.mempool.read().await;

        // Force all stats to 0
        let current_tps = 0.0;
        let total_transactions = 0;
        let tip_count = 0;

        Ok(DagStatsResponse {
            current_tps,
            total_transactions,
            tip_count,
            epoch: 0,
            connected_peers: consensus.get_all_validators().len() as u32,
        })
    }

    /// Get network hashrate (simplified placeholder for now)
    pub async fn get_network_hashrate(&self) -> Result<HashrateResponse, RpcError> {
        // For now, return a placeholder based on base difficulty
        // In production, this would calculate from recent transaction nonces
        let estimated_hashrate = 100 * 1000; // difficulty * 1000
        
        Ok(HashrateResponse {
            hashrate: format!("{} H/s", estimated_hashrate),
            difficulty: 100, // Base difficulty
        })
    }

    /// Determine transaction status based on actual protocol state
    /// Economic policy: separates local knowledge from global consensus
    pub async fn determine_transaction_status(&self, tx_id: TransactionId) -> TransactionStatus {
        // Step 1: Determine local status (what this node knows)
        let local_status = {
            // Check if in orphans (waiting for parents)
            {
                let orphans = self.orphans.read().await;
                if orphans.contains_key(&tx_id) {
                    LocalStatus::Orphan
                } else {
                    // Check if in mempool (accepted locally but not in DAG)
                    let mempool = self.mempool.read().await;
                    if mempool.get_transaction_ids().contains(&tx_id) {
                        LocalStatus::InMempool
                    } else {
                        // Check if in DAG
                        let dag = self.dag.read().await;
                        if dag.get_transaction(tx_id).is_some() {
                            LocalStatus::InLocalDag
                        } else {
                            // Unknown to this node (may exist elsewhere)
                            LocalStatus::Unknown
                        }
                    }
                }
            }
        };
        
        // Step 2: Determine consensus status (global stability)
        // Only meaningful if transaction is in local DAG
        let consensus_status = if local_status == LocalStatus::InLocalDag {
            let dag = self.dag.read().await;
            if let Some(tx) = dag.get_transaction(tx_id) {
                let reference_count = dag.children().get(&tx_id)
                    .map(|children| children.len())
                    .unwrap_or(0);
                
                // Check for finalized status (VQV consensus votes)
                // For now, we don't have a finalized mechanism, so we skip this
                // In future, this would check VQV votes or other consensus confirmation
                
                // Check for stable status (high weight)
                if tx.weight >= ConsensusStatus::STABILITY_THRESHOLD {
                    ConsensusStatus::Stable
                } else if reference_count >= ConsensusStatus::MIN_CONFIRMATIONS {
                    ConsensusStatus::Confirmed
                } else {
                    ConsensusStatus::Unconfirmed
                }
            } else {
                // Should not happen if local_status is InLocalDag
                ConsensusStatus::Unconfirmed
            }
        } else {
            // Not in local DAG, so consensus status is unknown/unconfirmed
            ConsensusStatus::Unconfirmed
        };
        
        TransactionStatus::new(local_status, consensus_status)
    }
    
    /// Get transaction status
    pub async fn get_transaction_status(&self, hash: TransactionId) -> Result<TransactionStatusResponse, RpcError> {
        let status = self.determine_transaction_status(hash).await;
        
        let dag = self.dag.read().await;
        let (reference_count, weight, timestamp) = match dag.get_transaction(hash) {
            Some(tx) => {
                let ref_count = dag.children().get(&hash)
                    .map(|children| children.len())
                    .unwrap_or(0);
                (ref_count, tx.weight, Some(tx.timestamp))
            },
            None => (0, 0.0, None),
        };
        
        Ok(TransactionStatusResponse {
            tx_id: hash,
            local_status: status.local_status.as_str().to_string(),
            consensus_status: status.consensus_status.as_str().to_string(),
            global_status: status.global_status.as_str().to_string(),
            confirmed: status.global_status == GlobalStatus::Confirmed 
                || status.global_status == GlobalStatus::Stable 
                || status.global_status == GlobalStatus::Finalized,
            practically_final: status.practically_final,
            block_height: Some(0), // DAG doesn't have block heights
            timestamp,
            reference_count,
            weight,
        })
    }

    /// Get recent transactions for explorer
    pub async fn get_recent_transactions(&self, limit: u64) -> Result<RecentTransactionsResponse, RpcError> {
        let dag = self.dag.read().await;
        let transactions: Vec<&Transaction> = dag.transactions().values().collect();

        let limit = limit.min(50) as usize;
        let recent_txs: Vec<TransactionInfo> = transactions
            .iter()
            .take(limit)
            .map(|tx| TransactionInfo {
                tx_id: tx.id,
                sender: tx.sender,
                receiver: tx.receiver,
                amount: tx.amount,
                fee: tx.fee,
                parents: tx.parents,
                timestamp: tx.timestamp,
                status: "confirmed".to_string(),
            })
            .collect();

        Ok(RecentTransactionsResponse {
            transactions: recent_txs,
            total_count: transactions.len() as u64,
        })
    }

    /// Get transaction history for a specific address
    pub async fn get_transaction_history(&self, address: String) -> Result<TransactionHistoryResponse, RpcError> {
        let dag = self.dag.read().await;
        
        // Decode address from hex
        let address_bytes = hex::decode(&address)
            .map_err(|_| RpcError("Invalid address hex".to_string()))?;
        let address_array: [u8; 32] = address_bytes.try_into()
            .map_err(|_| RpcError("Invalid address length".to_string()))?;
        
        // Filter transactions where address is sender or receiver
        let transactions: Vec<TransactionHistoryItem> = dag.transactions()
            .values()
            .filter(|tx| tx.sender == address_array || tx.receiver == address_array)
            .map(|tx| {
                let is_incoming = tx.receiver == address_array;
                TransactionHistoryItem {
                    hash: hex::encode(tx.id),
                    sender: hex::encode(tx.sender),
                    receiver: hex::encode(tx.receiver),
                    amount: tx.amount,
                    timestamp: tx.timestamp,
                    is_incoming,
                }
            })
            .collect();
        
        let total_count = transactions.len();
        
        Ok(TransactionHistoryResponse {
            transactions,
            total_count,
        })
    }

    /// Stake tokens for an address
    pub async fn stake_tokens(&self, address: &Address, amount: u64) -> Result<StakingResponse, RpcError> {
        tracing::info!("🔒 Stake request: address={}, amount={}", hex::encode(address), amount);
        let storage = self.storage.read().await;
        
        match storage.stake_tokens(*address, amount) {
            Ok(_) => {
                let staked_amount = storage.get_staked_amount(*address).unwrap_or(0);
                let rewards = storage.calculate_staking_reward(*address).unwrap_or(0);
                tracing::info!("✅ Stake successful: staked_amount={}, rewards={}", staked_amount, rewards);
                Ok(StakingResponse {
                    address: *address,
                    staked_amount,
                    rewards_earned: rewards,
                    success: true,
                })
            }
            Err(e) => {
                tracing::error!("❌ Stake failed: {}", e);
                Ok(StakingResponse {
                    address: *address,
                    staked_amount: 0,
                    rewards_earned: 0,
                    success: false,
                })
            },
        }
    }

    /// Unstake tokens for an address
    pub async fn unstake_tokens(&self, address: &Address) -> Result<StakingResponse, RpcError> {
        let storage = self.storage.read().await;
        
        match storage.unstake_tokens(*address) {
            Ok(total_return) => {
                Ok(StakingResponse {
                    address: *address,
                    staked_amount: 0,
                    rewards_earned: total_return,
                    success: true,
                })
            }
            Err(e) => Ok(StakingResponse {
                address: *address,
                staked_amount: 0,
                rewards_earned: 0,
                success: false,
            }),
        }
    }

    /// Get staking info for an address
    pub async fn get_staking_info(&self, address: &Address) -> Result<StakingInfoResponse, RpcError> {
        let storage = self.storage.read().await;
        
        if storage.has_staked_tokens(*address) {
            let staked_amount = storage.get_staked_amount(*address).unwrap_or(0);
            let rewards = storage.calculate_staking_reward(*address).unwrap_or(0);
            Ok(StakingInfoResponse {
                staked_amount,
                rewards_earned: rewards,
                has_staked: true,
            })
        } else {
            Ok(StakingInfoResponse {
                staked_amount: 0,
                rewards_earned: 0,
                has_staked: false,
            })
        }
    }

    /// Get account nonce for an address
    pub async fn get_account_nonce(&self, address: &Address) -> Result<AccountNonceResponse, RpcError> {
        let ledger = self.ledger.read().await;
        let current_nonce = ledger.get_nonce(address);
        let next_nonce = current_nonce + 1;
        drop(ledger);
        
        Ok(AccountNonceResponse {
            address: *address,
            current_nonce,
            next_nonce,
        })
    }

    /// Get DAG graph for visualization
    pub async fn get_dag_graph(&self) -> Result<DagGraphResponse, RpcError> {
        let dag = self.dag.read().await;
        let transactions: Vec<&Transaction> = dag.transactions().values().collect();

        let nodes: Vec<DagNode> = transactions
            .iter()
            .map(|tx| DagNode {
                tx_id: tx.id,
                sender: tx.sender,
                receiver: tx.receiver,
                amount: tx.amount,
                timestamp: tx.timestamp,
                weight: tx.weight,
            })
            .collect();

        let mut edges: Vec<DagEdge> = Vec::new();
        for tx in transactions.iter() {
            for parent in &tx.parents {
                if !parent.is_empty() {
                    edges.push(DagEdge {
                        from: *parent,
                        to: tx.id,
                    });
                }
            }
        }

        Ok(DagGraphResponse {
            nodes,
            edges,
            total_transactions: transactions.len(),
        })
    }

    /// Get tips from the DAG
    pub async fn get_tips(&self) -> Result<TipsResponse, RpcError> {
        let dag = self.dag.read().await;

        // Get tips (transactions with no children)
        // Only return transactions that exist in the DAG and have no children
        let mut tips: Vec<TransactionId> = dag.transactions()
            .values()
            .filter(|tx| !dag.children().contains_key(&tx.id))
            .map(|tx| tx.id)
            .collect();

        // If no tips found, return GENESIS_HASH as default tip
        if tips.is_empty() {
            tracing::warn!("get_tips: No tips found in DAG, returning GENESIS_HASH as default");
            tips.push([0u8; 32]); // GENESIS_HASH
        }

        let count = tips.len();

        tracing::debug!("get_tips: Returning {} tips out of {} total transactions", count, dag.transaction_count());

        Ok(TipsResponse {
            tips,
            count,
        })
    }

    /// Get DAG snapshot for explorer
    pub async fn get_dag_snapshot(&self) -> Result<DagSnapshotResponse, RpcError> {
        let dag = self.dag.read().await;

        // Helper function to calculate cumulative weight
        fn calculate_cumulative_weight(id: TransactionId, dag: &DAG) -> f64 {
            let mut visited = std::collections::HashSet::new();
            let mut queue = vec![id];
            let mut weight = 0u64;

            while let Some(current_id) = queue.pop() {
                if !visited.insert(current_id) {
                    continue;
                }

                weight += 1;

                // Add children to queue
                if let Some(children) = dag.children().get(&current_id) {
                    for child in children {
                        queue.push(*child);
                    }
                }
            }

            weight as f64
        }

        // Get last 100 transactions
        let transactions: Vec<TransactionSnapshot> = dag.transactions()
            .values()
            .take(100)
            .map(|tx| {
                let cumulative_weight = calculate_cumulative_weight(tx.id, &dag);
                let signature_valid = crate::wallet::Wallet::verify_transaction(tx);
                
                TransactionSnapshot {
                    hash: hex::encode(tx.id),
                    parents: tx.parents.iter().map(|p| hex::encode(p)).collect(),
                    cumulative_weight,
                    signature_valid,
                    sender: hex::encode(tx.sender),
                    receiver: hex::encode(tx.receiver),
                    amount: tx.amount,
                    fee: tx.fee,
                    nonce: tx.nonce,
                    timestamp: tx.timestamp,
                }
            })
            .collect();

        let snapshots = transactions;
        let count = snapshots.len();

        tracing::debug!("get_dag_snapshot: Returning {} transactions", count);

        Ok(DagSnapshotResponse {
            transactions: snapshots,
            count,
        })
    }

    /// Get mining status
    pub async fn get_mining_status(&self) -> Result<MiningStatusResponse, RpcError> {
        let is_mining = *self.mining_enabled.read().await;
        // For now, return a placeholder hashrate
        let hashrate = if is_mining { "1000 H/s" } else { "0 H/s" };
        
        Ok(MiningStatusResponse {
            is_mining,
            hashrate: hashrate.to_string(),
        })
    }

    /// Start mining
    pub async fn start_mining(&self) -> Result<String, RpcError> {
        let mut mining = self.mining_enabled.write().await;
        *mining = true;
        Ok("Mining started".to_string())
    }

    /// Stop mining
    pub async fn stop_mining(&self) -> Result<String, RpcError> {
        let mut mining = self.mining_enabled.write().await;
        *mining = false;
        Ok("Mining stopped".to_string())
    }

    /// Faucet - give test funds to an address
    pub async fn faucet(&self, address: Address) -> Result<FaucetResponse, RpcError> {
        let amount = 100_000_000_000u64; // 100 AETHER in smallest unit
        
        let mut ledger = self.ledger.write().await;
        let current_balance = ledger.get_balance(&address);
        if let Err(e) = ledger.add_balance(&address, amount) {
            tracing::error!("❌ Faucet: Failed to add balance: {}", e);
            return Err(RpcError(format!("Faucet failed: {}", e)));
        }
        
        tracing::info!("💰 Faucet: Added {} to address {} (previous balance: {})", 
            amount, hex::encode(address), current_balance);
        
        // Save ledger
        drop(ledger);
        let _ = self.save_tx.send(crate::SyncEvent::SaveRequested).await;
        
        Ok(FaucetResponse {
            success: true,
            amount,
            message: format!("Successfully sent {} AETHER to {}", amount / 1_000_000_000, hex::encode(address)),
        })
    }

    /// Create a new account (wallet)
    pub async fn create_account(&self) -> Result<CreateAccountResponse, RpcError> {
        use crate::wallet::Wallet;
        
        let wallet = Wallet::new();
        let address = hex::encode(wallet.address());
        let public_key = hex::encode(wallet.public_key_bytes());
        let private_key = hex::encode(wallet.secret_key_bytes());
        
        tracing::info!("🔑 New account created: {}", address);
        
        Ok(CreateAccountResponse {
            success: true,
            address,
            public_key,
            private_key,
            message: "Account created successfully".to_string(),
        })
    }
}

/// Start RPC server
pub async fn start_rpc_server(
    addr: SocketAddr,
    consensus: Arc<RwLock<VQVConsensus>>,
    dag: Arc<RwLock<DAG>>,
    ledger: Arc<RwLock<Ledger>>,
    storage: Arc<RwLock<crate::storage::Storage>>,
    ledger_path: std::path::PathBuf,
    mempool: Arc<RwLock<Mempool>>,
    p2p_network: Arc<crate::p2p::P2PNetwork>,
    save_tx: mpsc::Sender<SyncEvent>,
    mining_enabled: Arc<RwLock<bool>>,
    miner_address: Option<Address>,
    orphans: Arc<RwLock<std::collections::HashMap<[u8; 32], Transaction>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let rpc_impl = Arc::new(AetherRpcImpl::new(consensus, dag, ledger, storage, ledger_path, mempool.clone(), p2p_network, save_tx, mining_enabled, miner_address, orphans));
    
    // Log mempool config
    let mempool_config = {
        let mempool_read = mempool.read().await;
        (mempool_read.max_size(), mempool_read.semaphore().available_permits())
    };
    
    tracing::info!("🚀 Starting RPC server on http://{}", addr);
    tracing::info!("📊 Mempool: max_size={}, available_permits={}", mempool_config.0, mempool_config.1);

    // 1. RPC Route - POST only for JSON-RPC
    let rpc_route = Router::new()
        .route("/", post(handle_rpc))
        .with_state(rpc_impl.clone());

    // 2. UI Route - GET only for explorer
    let ui_route = Router::new()
        .route("/explorer", get(handle_explorer))
        .fallback(get(handle_fallback));

    // 3. Merge routes without conflict
    let app = Router::new()
        .merge(rpc_route)
        .merge(ui_route)
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("✅ RPC + Explorer server listening on http://{}", addr);
    
    axum::serve(listener, app).await?;
    
    Ok(())
}

/// Handle JSON-RPC POST requests
async fn handle_rpc(
    State(rpc_impl): State<Arc<AetherRpcImpl>>,
    Json(payload): Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    let method = payload.get("method").and_then(|m: &serde_json::Value| m.as_str()).unwrap_or("");
    let id = payload.get("id").cloned().unwrap_or(serde_json::Value::Null);
    
    let result = match method {
        "aether_getBalance" => {
            let addr = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|a: &serde_json::Value| a.as_str());
            match addr {
                Some(addr_str) => {
                    match hex::decode(addr_str) {
                        Ok(bytes) => {
                            match bytes.try_into() {
                                Ok(addr) => {
                                    match rpc_impl.get_balance(addr).await {
                                        Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(_) => Err(RpcError("Invalid address".to_string())),
                            }
                        }
                        Err(_) => Err(RpcError("Invalid hex address".to_string())),
                    }
                }
                None => Err(RpcError("Missing address parameter".to_string())),
            }
        }
        "aether_sendTransaction" => {
            let params = payload.get("params").cloned().unwrap_or(serde_json::Value::Array(vec![]));
            match rpc_impl.send_transaction(params).await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_getDagStats" => {
            match rpc_impl.get_dag_stats().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_getNetworkHashrate" => {
            match rpc_impl.get_network_hashrate().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_getTransactionStatus" => {
            let hash = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|h: &serde_json::Value| h.as_str());
            match hash {
                Some(hash_str) => {
                    match hex::decode(hash_str) {
                        Ok(bytes) => {
                            match bytes.try_into() {
                                Ok(hash) => {
                                    match rpc_impl.get_transaction_status(hash).await {
                                        Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(_) => Err(RpcError("Invalid hash".to_string())),
                            }
                        }
                        Err(_) => Err(RpcError("Invalid hex hash".to_string())),
                    }
                }
                None => Err(RpcError("Missing hash parameter".to_string())),
            }
        }
        "aether_getRecentTransactions" => {
            let limit = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|l: &serde_json::Value| l.as_u64()).unwrap_or(10);
            match rpc_impl.get_recent_transactions(limit).await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_getTransactionHistory" => {
            let address = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|a: &serde_json::Value| a.as_str());
            match address {
                Some(addr_str) => {
                    match rpc_impl.get_transaction_history(addr_str.to_string()).await {
                        Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                        Err(e) => Err(e),
                    }
                }
                None => Err(RpcError("Missing address parameter".to_string())),
            }
        }
        "aether_getDagGraph" => {
            match rpc_impl.get_dag_graph().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_stakeTokens" => {
            tracing::info!("🔒 Received aether_stakeTokens RPC call");
            let addr = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|a: &serde_json::Value| a.as_str());
            let amount = payload.get("params").and_then(|p: &serde_json::Value| p.get(1)).and_then(|a: &serde_json::Value| a.as_u64());
            tracing::info!("🔒 Parsed params: addr={:?}, amount={:?}", addr, amount);
            match (addr, amount) {
                (Some(addr_str), Some(amt)) => {
                    match hex::decode(addr_str) {
                        Ok(bytes) => {
                            match bytes.try_into() {
                                Ok(addr) => {
                                    tracing::info!("🔒 Calling stake_tokens implementation");
                                    match rpc_impl.stake_tokens(&addr, amt).await {
                                        Ok(response) => {
                                            tracing::info!("🔒 stake_tokens returned: {:?}", response);
                                            serde_json::to_value(response).map_err(|e| RpcError(e.to_string()))
                                        },
                                        Err(e) => {
                                            tracing::error!("🔒 stake_tokens error: {:?}", e);
                                            Err(e)
                                        },
                                    }
                                }
                                Err(_) => {
                                    tracing::error!("🔒 Invalid address conversion");
                                    Err(RpcError("Invalid address".to_string()))
                                },
                            }
                        }
                        Err(_) => {
                            tracing::error!("🔒 Invalid hex address");
                            Err(RpcError("Invalid hex address".to_string()))
                        },
                    }
                }
                _ => {
                    tracing::error!("🔒 Missing address or amount parameter");
                    Err(RpcError("Missing address or amount parameter".to_string()))
                },
            }
        }
        "aether_unstakeTokens" => {
            let addr = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|a: &serde_json::Value| a.as_str());
            match addr {
                Some(addr_str) => {
                    match hex::decode(addr_str) {
                        Ok(bytes) => {
                            match bytes.try_into() {
                                Ok(addr) => {
                                    match rpc_impl.unstake_tokens(&addr).await {
                                        Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(_) => Err(RpcError("Invalid address".to_string())),
                            }
                        }
                        Err(_) => Err(RpcError("Invalid hex address".to_string())),
                    }
                }
                None => Err(RpcError("Missing address parameter".to_string())),
            }
        }
        "aether_getStakingInfo" => {
            let addr = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|a: &serde_json::Value| a.as_str());
            match addr {
                Some(addr_str) => {
                    match hex::decode(addr_str) {
                        Ok(bytes) => {
                            match bytes.try_into() {
                                Ok(addr) => {
                                    match rpc_impl.get_staking_info(&addr).await {
                                        Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(_) => Err(RpcError("Invalid address".to_string())),
                            }
                        }
                        Err(_) => Err(RpcError("Invalid hex address".to_string())),
                    }
                }
                None => Err(RpcError("Missing address parameter".to_string())),
            }
        }
        "aether_getAccountNonce" => {
            let addr = payload.get("params").and_then(|p: &serde_json::Value| p.get(0)).and_then(|a: &serde_json::Value| a.as_str());
            match addr {
                Some(addr_str) => {
                    match hex::decode(addr_str) {
                        Ok(bytes) => {
                            match bytes.try_into() {
                                Ok(addr) => {
                                    match rpc_impl.get_account_nonce(&addr).await {
                                        Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(_) => Err(RpcError("Invalid address".to_string())),
                            }
                        }
                        Err(_) => Err(RpcError("Invalid hex address".to_string())),
                    }
                }
                None => Err(RpcError("Missing address parameter".to_string())),
            }
        }
        "aether_getTips" => {
            match rpc_impl.get_tips().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_getDagSnapshot" => {
            match rpc_impl.get_dag_snapshot().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_getMiningStatus" => {
            match rpc_impl.get_mining_status().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_startMining" => {
            match rpc_impl.start_mining().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_stopMining" => {
            match rpc_impl.stop_mining().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        "aether_faucet" => {
            match payload.get("params").and_then(|p: &serde_json::Value| p.get(0)) {
                Some(address_str) => {
                    match hex::decode(address_str.as_str().unwrap_or("")) {
                        Ok(bytes) if bytes.len() == 32 => {
                            let mut address = [0u8; 32];
                            address.copy_from_slice(&bytes);
                            match rpc_impl.faucet(address).await {
                                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                                Err(e) => Err(e),
                            }
                        }
                        _ => Err(RpcError("Invalid address format".to_string())),
                    }
                }
                None => Err(RpcError("Missing address parameter".to_string())),
            }
        }
        "aether_createAccount" => {
            match rpc_impl.create_account().await {
                Ok(response) => serde_json::to_value(response).map_err(|e| RpcError(e.to_string())),
                Err(e) => Err(e),
            }
        }
        _ => Err(RpcError(format!("Method not found: {}", method))),
    };
    
    match result {
        Ok(response) => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "result": response,
            "id": id
        })),
        Err(e) => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -1,
                "message": e.0
            },
            "id": id
        })),
    }
}

/// Handle explorer GET request
async fn handle_explorer() -> Html<&'static str> {
    Html(r#"<!DOCTYPE html>
<html>
<head>
    <title>AETHER SEDC Explorer</title>
    <meta charset="utf-8">
</head>
<body>
    <h1>AETHER SEDC Explorer</h1>
    <p>Explorer interface coming soon...</p>
</body>
</html>"#)
}

/// Handle fallback - redirect to explorer
async fn handle_fallback() -> Html<&'static str> {
    Html(r#"<!DOCTYPE html>
<html>
<head><meta http-equiv="refresh" content="0;url=/explorer"></head>
<body>Redirecting to explorer...</body>
</html>"#)
}
