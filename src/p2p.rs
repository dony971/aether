//! # P2P Networking Module
//!
//! Implements peer-to-peer networking for the Aether DAG with efficient inventory synchronization.
//! Uses tip-based synchronization and hash comparison for inventory management.

use crate::transaction::Transaction;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn, error, debug};

/// P2P network configuration
#[derive(Clone)]
pub struct P2PConfig {
    /// Local address to bind to
    pub listen_addr: SocketAddr,
    /// Bootstrap nodes to connect to
    pub bootnodes: Vec<SocketAddr>,
}

impl Default for P2PConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:30333".parse().unwrap_or_else(|_| {
                tracing::warn!("Failed to parse default P2P address, using fallback");
                "127.0.0.1:30333".parse().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap())
            }),
            bootnodes: Vec::new(),
        }
    }
}

/// P2P network message types
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum P2PMessage {
    /// Transaction gossip
    Transaction(Vec<u8>),
    /// Inventory - list of transaction hashes
    Inventory(Vec<Vec<u8>>),
    /// GetData - request specific transactions by hash
    GetData(Vec<Vec<u8>>),
    /// GetInventory - request inventory based on tips
    GetInventory {
        /// Tips of the requesting node
        tips: Vec<Vec<u8>>,
    },
    /// SyncRequest - request full inventory from peer (legacy)
    SyncRequest,
    /// SyncResponse - send transactions (with pagination support)
    SyncResponse(Vec<Vec<u8>>),
    /// Ping message for keepalive
    Ping,
    /// Pong response
    Pong,
}

/// Seen transaction entry with timestamp for LRU eviction
#[derive(Clone)]
struct SeenTxEntry {
    timestamp: Instant,
    source: TxSource,
}

/// Transaction source for better deduplication logic
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TxSource {
    Local,  // Transaction created locally via RPC
    Network, // Transaction received from P2P network
}

/// P2P network manager
#[derive(Clone)]
pub struct P2PNetwork {
    config: P2PConfig,
    peers: Arc<RwLock<HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>>>,
    // Channel to pass transactions to DAG engine
    tx_channel: mpsc::UnboundedSender<Transaction>,
    // Callback to get current DAG transaction hashes
    get_dag_hashes: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
    // Callback to get transaction by hash
    get_transaction_by_hash: Arc<dyn Fn(&[u8]) -> Option<Transaction> + Send + Sync>,
    // Callback to get balance for address
    get_balance: Arc<dyn Fn(&[u8; 32]) -> u64 + Send + Sync>,
    // Callback to save DAG (force save after P2P transaction)
    save_dag: Arc<dyn Fn() + Send + Sync>,
    // Callback to process orphans after transaction received
    process_orphans: Arc<dyn Fn() + Send + Sync>,
    // Callback to get current tips from DAG
    get_tips: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
    // Seen transactions for deduplication with timestamps
    seen_transactions: Arc<RwLock<HashMap<Vec<u8>, SeenTxEntry>>>,
}

impl P2PNetwork {
    /// Create a new P2P network manager
    pub fn new(
        config: P2PConfig,
        tx_channel: mpsc::UnboundedSender<Transaction>,
        get_dag_hashes: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
        get_transaction_by_hash: Arc<dyn Fn(&[u8]) -> Option<Transaction> + Send + Sync>,
        get_balance: Arc<dyn Fn(&[u8; 32]) -> u64 + Send + Sync>,
        save_dag: Arc<dyn Fn() + Send + Sync>,
        process_orphans: Arc<dyn Fn() + Send + Sync>,
        get_tips: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
    ) -> Self {
        Self {
            config,
            peers: Arc::new(RwLock::new(HashMap::new())),
            tx_channel,
            get_dag_hashes,
            get_transaction_by_hash,
            get_balance,
            save_dag,
            process_orphans,
            get_tips,
            seen_transactions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start the P2P network
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting P2P network on {}", self.config.listen_addr);

        // Start listening for incoming connections
        let listener = TcpListener::bind(self.config.listen_addr).await?;
        let peers = self.peers.clone();
        let tx_channel = self.tx_channel.clone();
        let get_dag_hashes = Arc::clone(&self.get_dag_hashes);
        let get_transaction_by_hash = Arc::clone(&self.get_transaction_by_hash);
        let get_balance = Arc::clone(&self.get_balance);
        let save_dag = Arc::clone(&self.save_dag);
        let process_orphans = Arc::clone(&self.process_orphans);
        let get_tips = Arc::clone(&self.get_tips);
        let seen_transactions = Arc::clone(&self.seen_transactions);
        tokio::spawn(async move {
            Self::accept_loop(listener, peers, tx_channel, get_dag_hashes, get_transaction_by_hash, get_balance, save_dag, process_orphans, get_tips, seen_transactions).await;
        });

        // Connect to bootnodes
        for bootnode in &self.config.bootnodes {
            self.connect_to_peer(*bootnode).await;
        }

        // Start heartbeat task
        self.start_heartbeat().await;

        // Start reconnection task for bootnodes
        self.start_reconnection_task().await;

        // Start cache cleanup task
        self.start_cache_cleanup().await;

        Ok(())
    }

    /// Start heartbeat task to check peer connections and reconnect if needed
    async fn start_heartbeat(&self) {
        let peers = self.peers.clone();
        let bootnodes = self.config.bootnodes.clone();
        let p2p_network = self.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

                let peer_count = {
                    let peers_read = peers.read().await;
                    peers_read.len()
                };

                if peer_count == 0 && !bootnodes.is_empty() {
                    warn!("No connected peers, attempting to reconnect to bootnodes...");
                    for bootnode in &bootnodes {
                        info!("Reconnecting to bootnode: {}", bootnode);
                        p2p_network.connect_to_peer(*bootnode).await;
                    }
                } else {
                    debug!("Heartbeat: {} connected peers", peer_count);
                }
            }
        });
    }

    /// Start reconnection task for bootnodes with exponential backoff
    async fn start_reconnection_task(&self) {
        let peers = self.peers.clone();
        let bootnodes = self.config.bootnodes.clone();
        let p2p_network = self.clone();

        tokio::spawn(async move {
            for bootnode in bootnodes {
                let bootnode_addr = bootnode;
                let mut retry_count = 0;

                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                    let connected = {
                        let current_peers = peers.read().await;
                        current_peers.contains_key(&bootnode_addr)
                    };

                    if !connected {
                        let delay = std::cmp::min(2u64.pow(retry_count), 60); // Max 60 seconds
                        info!("Bootnode {} disconnected, reconnecting in {}s (attempt {})", bootnode_addr, delay, retry_count + 1);
                        tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;

                        p2p_network.connect_to_peer(bootnode_addr).await;
                        retry_count = std::cmp::min(retry_count + 1, 6); // Cap at 6 (max delay 64s)
                    } else {
                        retry_count = 0; // Reset retry count if connected
                    }
                }
            }
        });
    }

    /// Start cache cleanup task to evict old seen transactions
    async fn start_cache_cleanup(&self) {
        let seen_transactions = self.seen_transactions.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await; // Clean every 5 minutes

                let now = Instant::now();
                let mut seen = seen_transactions.write().await;
                let before = seen.len();

                // Remove entries older than 1 hour
                seen.retain(|_, entry| now.duration_since(entry.timestamp) < Duration::from_secs(3600));

                let after = seen.len();
                if before != after {
                    debug!("Cache cleanup: removed {} entries ({} -> {})", before - after, before, after);
                }
            }
        });
    }

    /// Accept loop for incoming connections
    async fn accept_loop(
        listener: TcpListener,
        peers: Arc<RwLock<HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>>>,
        tx_channel: mpsc::UnboundedSender<Transaction>,
        get_dag_hashes: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
        get_transaction_by_hash: Arc<dyn Fn(&[u8]) -> Option<Transaction> + Send + Sync>,
        get_balance: Arc<dyn Fn(&[u8; 32]) -> u64 + Send + Sync>,
        save_dag: Arc<dyn Fn() + Send + Sync>,
        process_orphans: Arc<dyn Fn() + Send + Sync>,
        get_tips: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
        seen_transactions: Arc<RwLock<HashMap<Vec<u8>, SeenTxEntry>>>,
    ) {
        loop {
            match listener.accept().await {
                Ok((socket, addr)) => {
                    info!("New peer connected: {}", addr);
                    let (msg_sender, msg_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
                    {
                        let mut peers = peers.write().await;
                        peers.insert(addr, msg_sender.clone());
                    }

                    let peers_clone = peers.clone();
                    let tx_channel = tx_channel.clone();
                    let get_dag_hashes = Arc::clone(&get_dag_hashes);
                    let get_transaction_by_hash = Arc::clone(&get_transaction_by_hash);
                    let get_balance = Arc::clone(&get_balance);
                    let save_dag = Arc::clone(&save_dag);
                    let process_orphans = Arc::clone(&process_orphans);
                    let get_tips = Arc::clone(&get_tips);
                    let seen_transactions = Arc::clone(&seen_transactions);
                    tokio::spawn(async move {
                        Self::handle_peer(socket, addr, peers_clone, tx_channel, get_dag_hashes, get_transaction_by_hash, get_balance, save_dag, process_orphans, get_tips, seen_transactions, msg_sender, msg_receiver).await;
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    /// Handle a peer connection
    async fn handle_peer(
        socket: TcpStream,
        addr: SocketAddr,
        peers: Arc<RwLock<HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>>>,
        tx_channel: mpsc::UnboundedSender<Transaction>,
        get_dag_hashes: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
        get_transaction_by_hash: Arc<dyn Fn(&[u8]) -> Option<Transaction> + Send + Sync>,
        get_balance: Arc<dyn Fn(&[u8; 32]) -> u64 + Send + Sync>,
        save_dag: Arc<dyn Fn() + Send + Sync>,
        process_orphans: Arc<dyn Fn() + Send + Sync>,
        get_tips: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>,
        seen_transactions: Arc<RwLock<HashMap<Vec<u8>, SeenTxEntry>>>,
        msg_sender: mpsc::UnboundedSender<Vec<u8>>,
        msg_receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        // Split socket into read and write halves
        let (mut reader, mut writer) = socket.into_split();

        // Spawn task to handle outgoing messages
        let mut msg_receiver = msg_receiver;
        tokio::spawn(async move {
            while let Some(msg) = msg_receiver.recv().await {
                let len = (msg.len() as u32).to_be_bytes();
                if writer.write_all(&len).await.is_err() {
                    warn!("Failed to send message to {}", addr);
                    break;
                }
                if writer.write_all(&msg).await.is_err() {
                    warn!("Failed to send message to {}", addr);
                    break;
                }
            }
        });

        // Send GetInventory with tips on connection
        let our_tips = get_tips();
        
        if let Ok(getinv_msg) = bincode::serialize(&P2PMessage::GetInventory {
            tips: our_tips.clone(),
        }) {
            info!("[Sync] Sending GetInventory with {} tips to {}", our_tips.len(), addr);
            let _ = msg_sender.send(getinv_msg);
        }

        loop {
            // Read message length (4 bytes)
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf).await {
                Ok(_) => {}
                Err(e) => {
                    warn!("Peer {} disconnected: {}", addr, e);
                    break;
                }
            }

            let len = u32::from_be_bytes(len_buf) as usize;

            // Read message body
            let mut msg_buf = vec![0u8; len];
            match reader.read_exact(&mut msg_buf).await {
                Ok(_) => {}
                Err(e) => {
                    warn!("Failed to read message from {}: {}", addr, e);
                    break;
                }
            }

            // Deserialize message
            match bincode::deserialize::<P2PMessage>(&msg_buf) {
                Ok(msg) => {
                    match msg {
                        P2PMessage::Transaction(tx_bytes) => {
                            // Deduplication: check if we've seen this transaction
                            {
                                let seen = seen_transactions.read().await;
                                if seen.contains_key(&tx_bytes) {
                                    continue;
                                }
                            }

                            // Deserialize transaction
                            if let Ok(tx) = bincode::deserialize::<Transaction>(&tx_bytes) {
                                // Mark as seen with timestamp and source (Network) before sending to channel
                                seen_transactions.write().await.insert(tx_bytes.clone(), SeenTxEntry { 
                                    timestamp: Instant::now(),
                                    source: TxSource::Network,
                                });

                                info!("Received transaction from {}: {}", addr, hex::encode(tx.id));
                                let _ = tx_channel.send(tx);

                                // Note: Full validation (PoW, signature, balance, nonce, etc.) 
                                // is now done in main.rs via process_transaction() to ensure
                                // consistency with RPC path
                            } else {
                                warn!("Failed to deserialize transaction from {}", addr);
                            }
                        }
                        P2PMessage::Inventory(hashes) => {
                            // Determine which hashes we need
                            let our_hashes = get_dag_hashes();
                            let our_hash_set: HashSet<Vec<u8>> = our_hashes.into_iter().collect();
                            let missing_hashes: Vec<Vec<u8>> = hashes
                                .into_iter()
                                .filter(|h| !our_hash_set.contains(h))
                                .collect();

                            // Request missing transactions via GetData
                            if !missing_hashes.is_empty() {
                                info!("Requesting {} missing transactions from {}", missing_hashes.len(), addr);
                                if let Ok(getdata_msg) = bincode::serialize(&P2PMessage::GetData(missing_hashes)) {
                                    let _ = msg_sender.send(getdata_msg);
                                }
                            }
                        }
                        P2PMessage::GetInventory { tips: peer_tips } => {
                            // Get our hashes and tips
                            let our_hashes = get_dag_hashes();
                            let our_tips = get_tips();
                            let _our_hash_set: HashSet<Vec<u8>> = our_hashes.into_iter().collect();
                            
                            // Find transactions we have that peer doesn't have (compare tips)
                            let peer_tips_set: HashSet<Vec<u8>> = peer_tips.into_iter().collect();
                            let our_tips_set: HashSet<Vec<u8>> = our_tips.into_iter().collect();
                            
                            // Transactions we need from peer (tips we don't have)
                            let missing_for_us: Vec<Vec<u8>> = peer_tips_set
                                .difference(&our_tips_set)
                                .cloned()
                                .collect();
                            
                            // Transactions peer might need from us (tips we have that they don't)
                            let missing_for_peer: Vec<Vec<u8>> = our_tips_set
                                .difference(&peer_tips_set)
                                .cloned()
                                .collect();
                            
                            info!("[Sync] GetInventory from {}: we need {} tips, peer might need {} tips", 
                                addr, missing_for_us.len(), missing_for_peer.len());
                            
                            // Send our tips that peer doesn't have (with pagination)
                            if !missing_for_peer.is_empty() {
                                const PAGE_SIZE: usize = 100;
                                let mut tx_bytes_list = Vec::new();
                                
                                for hash in missing_for_peer.iter().take(PAGE_SIZE) {
                                    if let Some(tx) = get_transaction_by_hash(hash) {
                                        if let Ok(bytes) = bincode::serialize(&tx) {
                                            tx_bytes_list.push(bytes);
                                        }
                                    }
                                }
                                
                                if !tx_bytes_list.is_empty() {
                                    info!("[Sync] Sending {} transactions to {}", tx_bytes_list.len(), addr);
                                    if let Ok(sync_resp_msg) = bincode::serialize(&P2PMessage::SyncResponse(tx_bytes_list)) {
                                        let _ = msg_sender.send(sync_resp_msg);
                                    }
                                }
                            }
                            
                            // Request missing transactions from peer
                            if !missing_for_us.is_empty() {
                                if let Ok(getdata_msg) = bincode::serialize(&P2PMessage::GetData(missing_for_us)) {
                                    let _ = msg_sender.send(getdata_msg);
                                }
                            }
                        }
                        P2PMessage::GetData(hashes) => {
                            // Send requested transactions with pagination (max 100 per response)
                            const PAGE_SIZE: usize = 100;
                            let mut tx_bytes_list = Vec::new();

                            for hash in hashes.iter().take(PAGE_SIZE) {
                                if let Some(tx) = get_transaction_by_hash(hash) {
                                    if let Ok(bytes) = bincode::serialize(&tx) {
                                        tx_bytes_list.push(bytes);
                                    }
                                }
                            }

                            if !tx_bytes_list.is_empty() {
                                info!("[Sync] Sending {} transactions to {}", tx_bytes_list.len(), addr);
                                if let Ok(sync_resp_msg) = bincode::serialize(&P2PMessage::SyncResponse(tx_bytes_list)) {
                                    let _ = msg_sender.send(sync_resp_msg);
                                }
                            }
                        }
                        P2PMessage::SyncRequest => {
                            // Respond with full inventory
                            let our_hashes = get_dag_hashes();
                            info!("[Sync] Sending inventory with {} hashes to {}", our_hashes.len(), addr);
                            if let Ok(inv_msg) = bincode::serialize(&P2PMessage::Inventory(our_hashes)) {
                                let _ = msg_sender.send(inv_msg);
                            }
                        }
                        P2PMessage::SyncResponse(tx_bytes_list) => {
                            // Download and add transactions in chronological order
                            let mut downloaded_count = 0;
                            let mut transactions: Vec<Transaction> = Vec::new();

                            for tx_bytes in tx_bytes_list {
                                // Deduplication check with new structure
                                {
                                    let seen = seen_transactions.read().await;
                                    if seen.contains_key(&tx_bytes) {
                                        continue;
                                    }
                                }

                                if let Ok(tx) = bincode::deserialize::<Transaction>(&tx_bytes) {
                                    transactions.push(tx);
                                    downloaded_count += 1;
                                }
                            }

                            // Sort by timestamp (chronological order)
                            transactions.sort_by_key(|tx| tx.timestamp);

                            // Add to DAG via channel (full validation will be done in main.rs)
                            for tx in transactions {
                                if let Ok(tx_bytes) = bincode::serialize(&tx) {
                                    seen_transactions.write().await.insert(tx_bytes, SeenTxEntry { 
                                        timestamp: Instant::now(),
                                        source: TxSource::Network,
                                    });
                                }
                                let _ = tx_channel.send(tx);

                                // Throttle: wait 50ms after each transaction to let ledger breathe
                                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                            }

                            if downloaded_count > 0 {
                                info!("[Sync] Downloaded {} missing transactions from {}", downloaded_count, addr);
                                // Note: process_orphans is now handled by the validation logic in main.rs
                            }
                        }
                        P2PMessage::Ping => {
                            // Respond with pong - skip for now, need sender
                        }
                        P2PMessage::Pong => {
                            // Ignore pong
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize message from {}: {}", addr, e);
                }
            }
        }

        // Remove peer from peers map on disconnect
        {
            let mut peers_lock = peers.write().await;
            peers_lock.remove(&addr);
        }
        info!("Peer {} removed from peers map", addr);
    }

    /// Connect to a peer
    async fn connect_to_peer(&self, addr: SocketAddr) {
        info!("Tentative de connexion au bootnode: {}", addr);

        match TcpStream::connect(addr).await {
            Ok(socket) => {
                info!("Connected to peer: {}", addr);
                let (msg_sender, msg_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
                {
                    let mut peers = self.peers.write().await;
                    peers.insert(addr, msg_sender.clone());
                }

                let peers = self.peers.clone();
                let tx_channel = self.tx_channel.clone();
                let get_dag_hashes = Arc::clone(&self.get_dag_hashes);
                let get_transaction_by_hash = Arc::clone(&self.get_transaction_by_hash);
                let get_balance = Arc::clone(&self.get_balance);
                let save_dag = Arc::clone(&self.save_dag);
                let process_orphans = Arc::clone(&self.process_orphans);
                let get_tips = Arc::clone(&self.get_tips);
                let seen_transactions = Arc::clone(&self.seen_transactions);
                tokio::spawn(async move {
                    Self::handle_peer(socket, addr, peers, tx_channel, get_dag_hashes, get_transaction_by_hash, get_balance, save_dag, process_orphans, get_tips, seen_transactions, msg_sender, msg_receiver).await;
                });
            }
            Err(e) => {
                warn!("Failed to connect to {}: {}", addr, e);
            }
        }
    }

    /// Broadcast a transaction to all connected peers
    pub async fn broadcast_transaction(&self, tx: Transaction) {
        let tx_bytes = match bincode::serialize(&tx) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to serialize transaction: {}", e);
                return;
            }
        };

        // Mark as seen with Local source before broadcasting to avoid rebroadcast loops
        self.seen_transactions.write().await.insert(tx_bytes.clone(), SeenTxEntry {
            timestamp: Instant::now(),
            source: TxSource::Local,
        });

        let msg = P2PMessage::Transaction(tx_bytes.clone());
        let msg_bytes = match bincode::serialize(&msg) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to serialize P2P message: {}", e);
                return;
            }
        };

        let peers = self.peers.read().await;
        let peers: &std::collections::HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>> = &*peers;
        for (addr, sender) in peers.iter() {
            let sender: &mpsc::UnboundedSender<Vec<u8>> = sender;
            if sender.send(msg_bytes.clone()).is_err() {
                warn!("Failed to send transaction to {}: channel closed", addr);
            }
        }
    }

    /// Get the number of connected peers
    pub async fn peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    /// Get the list of connected peers
    pub async fn get_peers(&self) -> Vec<SocketAddr> {
        let peers = self.peers.read().await;
        let peers: &std::collections::HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>> = &*peers;
        peers.keys().copied().collect()
    }

    /// Request a specific transaction by hash from all peers
    pub async fn request_transaction(&self, hash: Vec<u8>) {
        let msg = P2PMessage::GetData(vec![hash.clone()]);
        let msg_bytes = match bincode::serialize(&msg) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to serialize GetData message: {}", e);
                return;
            }
        };

        let peers = self.peers.read().await;
        for (addr, sender) in peers.iter() {
            if sender.send(msg_bytes.clone()).is_err() {
                warn!("Failed to send GetData request to {}: channel closed", addr);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transaction;
    use tokio::sync::mpsc;

    #[test]
    fn test_seen_tx_entry_source() {
        // Test that TxSource enum works correctly
        let local = TxSource::Local;
        let network = TxSource::Network;
        
        assert_eq!(local, TxSource::Local);
        assert_eq!(network, TxSource::Network);
        assert_ne!(local, network);
    }

    #[tokio::test]
    async fn test_seen_transactions_deduplication() {
        // Test that seen_transactions correctly tracks and deduplicates
        let seen = Arc::new(RwLock::new(HashMap::new()));
        
        let tx_bytes = vec![1u8, 2, 3, 4];
        
        // First insert should succeed
        {
            let mut seen_write = seen.write().await;
            seen_write.insert(tx_bytes.clone(), SeenTxEntry {
                timestamp: Instant::now(),
                source: TxSource::Network,
            });
        }
        
        // Check that it's present
        {
            let seen_read = seen.read().await;
            assert!(seen_read.contains_key(&tx_bytes));
            let entry = seen_read.get(&tx_bytes).unwrap();
            assert_eq!(entry.source, TxSource::Network);
        }
        
        // Second insert should not duplicate (just update)
        {
            let mut seen_write = seen.write().await;
            seen_write.insert(tx_bytes.clone(), SeenTxEntry {
                timestamp: Instant::now(),
                source: TxSource::Local,
            });
        }
        
        // Should still be only one entry
        {
            let seen_read = seen.read().await;
            assert_eq!(seen_read.len(), 1);
            let entry = seen_read.get(&tx_bytes).unwrap();
            // Source should be updated to Local
            assert_eq!(entry.source, TxSource::Local);
        }
    }

    #[tokio::test]
    async fn test_seen_transactions_cleanup() {
        // Test that old entries are evicted
        let seen = Arc::new(RwLock::new(HashMap::new()));
        
        let tx_bytes1 = vec![1u8, 2, 3, 4];
        let tx_bytes2 = vec![5u8, 6, 7, 8];
        
        // Insert one entry with old timestamp (use smaller duration to avoid overflow)
        {
            let mut seen_write = seen.write().await;
            let now = Instant::now();
            seen_write.insert(tx_bytes1.clone(), SeenTxEntry {
                timestamp: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
                source: TxSource::Network,
            });
            seen_write.insert(tx_bytes2.clone(), SeenTxEntry {
                timestamp: now,
                source: TxSource::Local,
            });
        }
        
        // Simulate cleanup (remove entries older than 30 seconds)
        let now = Instant::now();
        {
            let mut seen_write = seen.write().await;
            seen_write.retain(|_, entry| {
                now.checked_duration_since(entry.timestamp)
                    .map_or(false, |d| d < Duration::from_secs(30))
            });
        }
        
        // Old entry should be removed, new entry should remain
        {
            let seen_read = seen.read().await;
            assert!(!seen_read.contains_key(&tx_bytes1));
            assert!(seen_read.contains_key(&tx_bytes2));
            assert_eq!(seen_read.len(), 1);
        }
    }

    #[tokio::test]
    async fn test_broadcast_marks_as_local() {
        // Test that broadcast_transaction marks tx as Local before sending
        let (tx_channel, _tx_receiver) = mpsc::unbounded_channel::<Transaction>();
        let p2p = P2PNetwork::new(
            P2PConfig::default(),
            tx_channel,
            Arc::new(|| vec![]),
            Arc::new(|_| None),
            Arc::new(|_| 0),
            Arc::new(|| {}),
            Arc::new(|| {}),
            Arc::new(|| vec![]),
        );
        
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
        
        let tx_bytes = bincode::serialize(&tx).unwrap();
        
        // Broadcast should mark as Local
        p2p.broadcast_transaction(tx.clone()).await;
        
        // Check that it's marked as Local in seen_transactions
        let seen = p2p.seen_transactions.read().await;
        assert!(seen.contains_key(&tx_bytes));
        let entry = seen.get(&tx_bytes).unwrap();
        assert_eq!(entry.source, TxSource::Local);
    }
}
