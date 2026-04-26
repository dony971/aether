//! # Aether Node Main Entry Point
//!
//! Main entry point for the Aether blockchain node.
//! Defaults to mining mode if no command is specified.

use aether_unified::{
    transaction::{Transaction, Address},
    consensus::VQVConsensus,
    parent_selection::DAG,
    genesis::{initialize_genesis, GenesisConfig, GENESIS_MESSAGE},
    p2p::{P2PNetwork, P2PConfig},
    rpc::start_rpc_server,
    wallet::Wallet,
    json_storage::{save_dag_to_json, ensure_data_dir, load_dag_from_json},
    pow::{MicroPoW, DifficultyAdjuster},
    rpc::Mempool,
    // networking::P2PNetwork,  // Temporarily disabled for Windows build
};
use std::env;
use std::path::PathBuf;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use colored::*;

/// Synchronous ledger save function - uses spawn_blocking with Sled to avoid blocking Tokio runtime
async fn sync_save_ledger(ledger: Arc<RwLock<aether_unified::ledger::Ledger>>, ledger_path: PathBuf) {
    let ledger_clone = ledger.clone();
    tokio::task::spawn_blocking(move || {
        let ledger_lock = ledger_clone.blocking_read();
        ledger_lock.save_blocking(&ledger_path).ok();
    }).await.ok();
}

/// Parse bootnode address from either IP:PORT, DOMAIN:PORT, or multiaddr format
fn parse_bootnode_address(addr_str: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    // Try standard IP:PORT format first
    if let Ok(addr) = addr_str.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // Try DOMAIN:PORT format (e.g., 0.tcp.eu.ngrok.io:10979)
    if addr_str.contains(':') && !addr_str.starts_with('/') {
        let parts: Vec<&str> = addr_str.split(':').collect();
        if parts.len() == 2 {
            let domain = parts[0];
            let port = parts[1].parse::<u16>()?;
            // Resolve domain to IP
            if let Ok(mut ips) = std::net::ToSocketAddrs::to_socket_addrs(&format!("{}:{}", domain, port)) {
                if let Some(addr) = ips.next() {
                    return Ok(addr);
                }
            }
        }
    }

    // Try multiaddr format: /ip4/127.0.0.1/tcp/30333
    if addr_str.starts_with("/ip4/") || addr_str.starts_with("/ip6/") {
        let parts: Vec<&str> = addr_str.split('/').collect();
        if parts.len() >= 5 {
            // Format: /ip4/127.0.0.1/tcp/30333
            // parts[0] = "", parts[1] = "ip4", parts[2] = "127.0.0.1", parts[3] = "tcp", parts[4] = "30333"
            let ip = parts[2];
            let port = parts[4].parse::<u16>()?;
            let addr_str = format!("{}:{}", ip, port);
            return addr_str.parse::<SocketAddr>().map_err(|e| e.into());
        }
    }

    Err(format!("Invalid bootnode address format: {}. Use IP:PORT, DOMAIN:PORT, or /ip4/IP/tcp/PORT", addr_str).into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("🚀 Aether Node starting...");

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();

    // Check for keygen command (legacy)
    if args.len() > 1 && args[1] == "keygen" {
        let wallet_path = if args.len() > 2 { &args[2] } else { "wallet.json" };
        let wallet = Wallet::new();
        wallet.to_file(wallet_path, None).await?;
        println!("{}", "✓ Wallet generated and saved to".green());
        println!("  Path: {}", wallet_path.cyan());
        println!("  Address: {}", hex::encode(wallet.address()).cyan());
        return Ok(());
    }

    // Check for wallet create command (new with mnemonic)
    if args.len() > 1 && args[1] == "wallet" && args.len() > 2 && args[2] == "create" {
        let wallet_path = if args.len() > 3 { &args[3] } else { "wallet.json" };
        
        // Generate wallet with mnemonic
        let wallet = Wallet::new_with_mnemonic();
        
        // Prompt for password
        print!("Enter password to encrypt wallet: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut password = String::new();
        std::io::stdin().read_line(&mut password)?;
        let password = password.trim();
        
        if password.is_empty() {
            eprintln!("{}", "Password cannot be empty".red());
            std::process::exit(1);
        }
        
        // Save encrypted wallet
        wallet.to_file(wallet_path, Some(password)).await?;

        println!("{}", "✓ Wallet generated and saved".green());
        println!("  Path: {}", wallet_path.cyan());
        println!("  Address: {}", wallet.address_string().cyan());
        println!();
        println!("{}", "⚠️  IMPORTANT: Write down your mnemonic phrase below.".yellow());
        println!("   This is the ONLY way to recover your wallet if you lose the file.");
        println!();
        println!("   Mnemonic: {}", wallet.mnemonic.as_deref().unwrap_or("<unavailable>").cyan());
        println!();
        println!("{}", "   Store this phrase in a secure location. Never share it with anyone.".yellow());
        println!();
        return Ok(());
    }

    // Check for send command
    if args.len() > 1 && args[1] == "send" {
        if args.len() < 4 {
            eprintln!("Usage: aether-node.exe send <receiver_hex> <amount> <fee> [--rpc-url <url>] [--wallet <path>] [--password <password>]");
            eprintln!("Example: aether-node.exe send 7dc0a40351d86d28929f59786c039f7fcb73a215d6848c396230fc64d0578cf0 100 10");
            std::process::exit(1);
        }

        let receiver_hex = &args[2];
        let amount: u64 = args[3].parse()?;
        let fee: u64 = if args.len() > 4 { args[4].parse()? } else { 10 };
        
        // Default wallet detection - if --wallet not specified, use wallet.json
        let mut wallet_path = "wallet.json";
        let mut password: Option<String> = None;
        
        // Check if wallet.json exists, if not provide helpful message
        if !std::path::Path::new(wallet_path).exists() {
            eprintln!("{}", "⚠️  wallet.json not found in current directory".yellow());
            eprintln!("  Creating a new wallet with: aether-node wallet create");
            eprintln!("  Or specify wallet path with: --wallet <path>");
            std::process::exit(1);
        }
        
        let mut i = 5;
        while i < args.len() {
            match args[i].as_str() {
                "--rpc-url" => {
                    if i + 1 < args.len() {
                        // Will be handled later
                        i += 2;
                    }
                }
                "--wallet" => {
                    if i + 1 < args.len() {
                        wallet_path = &args[i + 1];
                        i += 2;
                    }
                }
                "--password" => {
                    if i + 1 < args.len() {
                        password = Some(args[i + 1].clone());
                        i += 2;
                    }
                }
                _ => { i += 1; }
            }
        }

        let mut rpc_url = "http://127.0.0.1:9933".to_string();
        let mut i = 5;
        while i < args.len() {
            match args[i].as_str() {
                "--rpc-url" => {
                    if i + 1 < args.len() {
                        rpc_url = args[i + 1].clone();
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                _ => { i += 1; }
            }
        }

        send_transaction_client(&wallet_path, receiver_hex, amount, fee, &rpc_url, password.as_deref()).await?;
        return Ok(());
    }

    // Check for balance command
    if args.len() > 1 && args[1] == "balance" {
        if args.len() < 2 {
            eprintln!("Usage: aether-node.exe balance <address_or_wallet_path> [--rpc-url <url>]");
            eprintln!("Example: aether-node.exe balance 7dc0a40351d86d28929f59786c039f7fcb73a215d6848c396230fc64d0578cf0");
            eprintln!("         aether-node.exe balance wallet.json");
            std::process::exit(1);
        }

        let address_input = if args.len() > 2 { &args[2] } else { "wallet.json" };
        
        // Default wallet detection for balance command
        if address_input == "wallet.json" && !std::path::Path::new(address_input).exists() {
            eprintln!("{}", "⚠️  wallet.json not found in current directory".yellow());
            eprintln!("  Creating a new wallet with: aether-node wallet create");
            eprintln!("  Or specify address directly: aether-node balance <address_hex>");
            std::process::exit(1);
        }
        
        let mut rpc_url = "http://127.0.0.1:9933".to_string();
        let mut password: Option<String> = None;

        let mut i = 3;
        while i < args.len() {
            match args[i].as_str() {
                "--rpc-url" => {
                    rpc_url = args[i + 1].clone();
                    i += 2;
                }
                "--password" => {
                    password = Some(args[i + 1].clone());
                    i += 2;
                }
                _ => {
                    i += 1;
                }
            }
        }

        // Check if input is a wallet file or an address
        let address_hex = if address_input.ends_with(".json") || std::path::Path::new(address_input).exists() {
            // Load wallet and get address
            let wallet: aether_unified::wallet::Wallet = Wallet::from_file(address_input, password.as_deref()).await?;
            let addr = hex::encode(wallet.address());
            println!("📍 Address: {}", addr);
            addr
        } else {
            // Assume it's an address
            address_input.to_string()
        };

        // Try to get balance from RPC, but don't fail if node is not running
        match balance_client(&address_hex, &rpc_url).await {
            Ok(_) => {},
            Err(_) => {
                println!("⚠️  Could not connect to RPC server at {}", rpc_url);
                println!("   Make sure the node is running to check balance");
            }
        }
        return Ok(());
    }

    // Default configuration
    let mut node_type = "miner".to_string(); // Default to mining mode
    let mut data_dir = PathBuf::from("./data");
    let mut p2p_port = 30333;
    let mut rpc_port = 9933;
    let mut bootnodes: Vec<SocketAddr> = Vec::new();
    let mut miner_address: Option<Address> = None;

    // Parse arguments
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--node-type" => {
                node_type = args[i + 1].clone();
                i += 2;
            }
            "--data-dir" => {
                data_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--p2p-port" => {
                p2p_port = args[i + 1].parse()?;
                i += 2;
            }
            "--rpc-port" => {
                rpc_port = args[i + 1].parse()?;
                i += 2;
            }
            "--bootnodes" => {
                let bootnodes_str = &args[i + 1];
                for addr_str in bootnodes_str.split(',') {
                    let addr_str = addr_str.trim();
                    let addr = parse_bootnode_address(addr_str)?;
                    bootnodes.push(addr);
                }
                i += 2;
            }
            "--miner-address" => {
                let addr_hex = &args[i + 1];
                let addr: Address = hex::decode(addr_hex)?
                    .try_into()
                    .map_err(|_| "Invalid miner address hex")?;
                miner_address = Some(addr);
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    tracing::debug!("Arguments parsed successfully");
    tracing::debug!("Node Type: {}", node_type);
    tracing::debug!("Data Directory: {:?}", data_dir);
    tracing::debug!("Bootnodes: {:?}", bootnodes);
    tracing::debug!("Miner Address: {:?}", miner_address.map(|a| hex::encode(a)));

    tracing::info!("🚀 Aether Node v1.0.0 - The Satoshi Protocol");
    tracing::info!("========================================");
    tracing::info!("Node Type: {}", node_type);
    tracing::info!("Data Directory: {:?}", data_dir);
    tracing::info!("P2P Port: {}", p2p_port);
    tracing::info!("RPC Port: {}", rpc_port);
    tracing::info!("Bootnodes: {:?}", bootnodes);

    // Check for reset flag (after parsing to use correct data-dir)
    if args.iter().any(|arg| arg == "--reset") {
        tracing::warn!("🗑️  --reset flag detected: Cleaning up storage data...");
        if data_dir.exists() {
            tracing::info!("  Deleting data directory: {:?}", data_dir);
            std::fs::remove_dir_all(&data_dir).map_err(|e| format!("Failed to delete data directory: {}", e))?;
            tracing::info!("  ✅ Data directory deleted successfully");
        } else {
            tracing::info!("  Data directory does not exist, nothing to delete");
        }
    }

    // Parse arguments
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--node-type" => {
                node_type = args[i + 1].clone();
                i += 2;
            }
            "--data-dir" => {
                data_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--p2p-port" => {
                p2p_port = args[i + 1].parse()?;
                i += 2;
            }
            "--rpc-port" => {
                rpc_port = args[i + 1].parse()?;
                i += 2;
            }
            "--bootnodes" => {
                let bootnodes_str = &args[i + 1];
                for addr_str in bootnodes_str.split(',') {
                    let addr_str = addr_str.trim();
                    let addr = parse_bootnode_address(addr_str)?;
                    bootnodes.push(addr);
                }
                i += 2;
            }
            "--miner-address" => {
                let addr_hex = &args[i + 1];
                let addr: Address = hex::decode(addr_hex)?
                    .try_into()
                    .map_err(|_| "Invalid miner address hex")?;
                miner_address = Some(addr);
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    tracing::debug!("Arguments parsed successfully");
    tracing::debug!("Node Type: {}", node_type);
    tracing::debug!("Data Directory: {:?}", data_dir);
    tracing::debug!("Bootnodes: {:?}", bootnodes);
    tracing::debug!("Miner Address: {:?}", miner_address.map(|a| hex::encode(a)));

    tracing::info!("🚀 Aether Node v1.0.0 - The Satoshi Protocol");
    tracing::info!("========================================");
    tracing::info!("Node Type: {}", node_type);
    tracing::info!("Data Directory: {:?}", data_dir);
    tracing::info!("P2P Port: {}", p2p_port);
    tracing::info!("RPC Port: {}", rpc_port);
    tracing::info!("Bootnodes: {:?}", bootnodes);

    // Check for reset flag (after parsing to use correct data-dir)
    if args.iter().any(|arg| arg == "--reset") {
        tracing::warn!("🗑️  --reset flag detected: Cleaning up storage data...");
        if data_dir.exists() {
            tracing::info!("  Deleting data directory: {:?}", data_dir);
            std::fs::remove_dir_all(&data_dir).map_err(|e| format!("Failed to delete data directory: {}", e))?;
            tracing::info!("  ✅ Data directory deleted successfully");
        } else {
            tracing::info!("  Data directory does not exist, nothing to delete");
        }
    }

    // Ensure data directory exists
    ensure_data_dir(&data_dir).await?;
    let db_path = data_dir.join("sled_db");
    let dag_store_path = data_dir.join("dag.json");
    let ledger_path = data_dir.join("ledger.json");

    // Initialize Sled storage
    tracing::info!("🗄️  Initializing Sled database at {:?}", db_path);
    let storage = Arc::new(RwLock::new(aether_unified::storage::Storage::open(&db_path)
        .map_err(|e| format!("Failed to open Sled database: {}", e))?));

    // Check if migration from JSON to Sled is needed
    {
        let storage_read = storage.read().await;
        if storage_read.needs_migration(&data_dir) {
            tracing::info!("🔄 Migration from JSON to Sled needed");
            drop(storage_read);
            let mut storage_write = storage.write().await;
            storage_write.migrate_from_json(&data_dir)
                .map_err(|e| format!("Failed to migrate from JSON: {}", e))?;
        }
    }

    // Load ledger from Sled (source of truth for balances and nonces)
    let mut ledger = aether_unified::ledger::Ledger::new_with_storage(storage.clone(), &ledger_path).await
        .map_err(|e| format!("Failed to load ledger from Sled: {}", e))?;

    // Initialize genesis balances from config if ledger is empty
    let genesis_config = GenesisConfig::default();
    let mut initialized_count = 0;
    for (addr, balance) in &genesis_config.initial_balances {
        let addr_hex = hex::encode(addr);
        let current_balance = ledger.get_balance_hex(&addr_hex);
        tracing::info!("🔍 Genesis check: {} -> current balance: {}", addr_hex, current_balance);
        tracing::info!("🔍 Genesis config balance for {}: {} (raw)", addr_hex, balance);
        // Only initialize if balance is 0 (either new address or existing with 0 balance)
        if current_balance == 0 {
            ledger.set_balance(addr, *balance);
            initialized_count += 1;
            tracing::info!("🌱 Genesis balance set for {}: {} AETH", addr_hex, *balance / 1_000_000_000_000_000_000);
        }
    }
    if initialized_count > 0 {
        tracing::info!("{}", format!("🌱 Genesis initialized: {} addresses with message: {}", initialized_count, GENESIS_MESSAGE).cyan());
        // Save to Sled synchronously to ensure balances are persisted before GUI connects
        let storage_read = storage.read().await;
        for (addr_hex, balance) in &ledger.balances {
            let addr_bytes = hex::decode(addr_hex)?;
            let address: Address = addr_bytes.as_slice().try_into()
                .map_err(|e| format!("Invalid address length: {}", e))?;
            storage_read.put_balance(address, *balance)?;
        }
        storage_read.flush()?;
        drop(storage_read);
        tracing::info!("💾 Genesis balances saved to Sled");
    } else {
        tracing::info!("✅ Genesis balances already initialized");
    }

    // Create MPSC channel for save worker
    let (save_tx, mut save_rx) = mpsc::channel::<aether_unified::SyncEvent>(100);

    // Try to load existing DAG state from JSON, or initialize genesis
    // DAG is reconstructed from JSON; ledger (balances+nonces) is the source of truth
    let (dag, consensus, balances, orphans, missing_parent_hashes) = if dag_store_path.exists() {
        tracing::info!("📂 Loading DAG state from JSON...");
        let store = load_dag_from_json(&dag_store_path).await?;
        tracing::info!("  Loaded {} transactions from storage", store.transactions.len());
        tracing::info!("  Loaded {} child relationships from storage", store.children.len());

        // Reconstruct DAG from stored data
        let mut dag = DAG::new();
        let consensus = VQVConsensus::new(100, 0.7, 5, 50);
        let mut orphans: std::collections::HashMap<[u8; 32], aether_unified::transaction::Transaction> = std::collections::HashMap::new();

        // First, add all transactions (check for missing parents)
        let mut skipped_count = 0;
        for stored_tx in store.transactions {
            let signature = if let Some(sig) = stored_tx.signature {
                hex::decode(&sig).unwrap_or_else(|_| {
                    tracing::warn!("Failed to decode signature for transaction, skipping");
                    vec![0u8; 64]
                })
            } else {
                vec![0u8; 64]
            };
            let public_key = if let Some(pk) = stored_tx.public_key {
                hex::decode(&pk).unwrap_or_else(|_| {
                    tracing::warn!("Failed to decode public key for transaction, skipping");
                    vec![0u8; 32]
                })
            } else {
                vec![0u8; 32]
            };

            let parent0_bytes = hex::decode(&stored_tx.parents[0]).unwrap_or_else(|_| {
                tracing::warn!("Failed to decode parent0 for transaction, skipping");
                vec![0u8; 32]
            });
            let parent1_bytes = hex::decode(&stored_tx.parents[1]).unwrap_or_else(|_| {
                tracing::warn!("Failed to decode parent1 for transaction, skipping");
                vec![0u8; 32]
            });
            let sender_bytes = hex::decode(&stored_tx.sender).unwrap_or_else(|_| {
                tracing::warn!("Failed to decode sender for transaction, skipping");
                vec![0u8; 32]
            });
            let receiver_bytes = hex::decode(&stored_tx.receiver).unwrap_or_else(|_| {
                tracing::warn!("Failed to decode receiver for transaction, skipping");
                vec![0u8; 32]
            });

            let parent0: aether_unified::transaction::TransactionId = parent0_bytes.clone().try_into().unwrap_or_else(|_| {
                tracing::warn!("Failed to convert parent0 to TransactionId, skipping transaction");
                [0u8; 32]
            });
            let parent1: aether_unified::transaction::TransactionId = parent1_bytes.clone().try_into().unwrap_or_else(|_| {
                tracing::warn!("Failed to convert parent1 to TransactionId, skipping transaction");
                [0u8; 32]
            });

            let sender: Address = sender_bytes.clone().try_into().unwrap_or_else(|_| {
                tracing::warn!("Failed to convert sender to Address, skipping transaction");
                [0u8; 32]
            });
            let receiver: Address = receiver_bytes.clone().try_into().unwrap_or_else(|_| {
                tracing::warn!("Failed to convert receiver to Address, skipping transaction");
                [0u8; 32]
            });

            // Skip if critical fields are invalid (all zeros except genesis)
            if parent0_bytes.iter().all(|&b| b == 0) && parent1_bytes.iter().all(|&b| b == 0) &&
               sender_bytes.iter().all(|&b| b == 0) && receiver_bytes.iter().all(|&b| b == 0) {
                tracing::warn!("Skipping transaction with all-zero critical fields");
                skipped_count += 1;
                continue;
            }

            // Check if parents exist in DAG (skip genesis hash check)
            let parent0_missing = parent0 != [0u8; 32] && !dag.transactions().contains_key(&parent0);
            let parent1_missing = parent1 != [0u8; 32] && !dag.transactions().contains_key(&parent1);

            if parent0_missing || parent1_missing {
                tracing::warn!("⚠️ Orphan transaction detected: missing parent(s) - tx_id: {}, parent0: {}, parent1: {}",
                    hex::encode(&parent0_bytes[..8]), // Log first 8 bytes for brevity
                    if parent0_missing { "MISSING" } else { "OK" },
                    if parent1_missing { "MISSING" } else { "OK" });
                // Add to orphan queue for later processing
                let orphan_tx = aether_unified::transaction::Transaction::new(
                    [parent0, parent1],
                    sender,
                    receiver,
                    stored_tx.amount,
                    stored_tx.fee,
                    stored_tx.timestamp,
                    stored_tx.nonce,
                    stored_tx.account_nonce, // Use as-is (0 for pre-nonce-era transactions)
                    signature,
                    public_key,
                );
                orphans.insert(orphan_tx.id, orphan_tx);
                continue; // Skip this transaction for now
            }

            let tx = aether_unified::transaction::Transaction::new(
                [parent0, parent1],
                sender_bytes.try_into().unwrap_or_else(|_| [0u8; 32]),
                receiver_bytes.try_into().unwrap_or_else(|_| [0u8; 32]),
                stored_tx.amount,
                stored_tx.fee,
                stored_tx.timestamp,
                stored_tx.nonce,
                stored_tx.account_nonce, // Use as-is (0 for pre-nonce-era transactions)
                signature,
                public_key,
            );
            // Load transaction from storage - use validated method for zero-trust
            dag.add_transaction_validated(tx.clone()).expect("Loaded transaction should be valid");

            // NOTE: Balances are NOT recalculated here. Ledger (loaded from Sled) is the source of truth.
            // This prevents inconsistency if crash occurs between DAG save and ledger save.
        }

        // Log orphan count and collect missing parent hashes
        let mut missing_parent_hashes: Vec<Vec<u8>> = Vec::new();
        if skipped_count > 0 {
            tracing::warn!("⚠️ Skipped {} invalid transactions during DAG loading", skipped_count);
        }
        if !orphans.is_empty() {
            tracing::warn!("⚠️ {} orphan transactions detected - requesting missing parents via P2P", orphans.len());
            for (_tx_id, orphan) in &orphans {
                for parent in orphan.parents.iter() {
                    if *parent != [0u8; 32] && !dag.transactions().contains_key(parent) {
                        missing_parent_hashes.push(parent.to_vec());
                        tracing::info!("📡 Orphan Solver - Requesting missing parent: {}", hex::encode(parent));
                    }
                }
            }
        }

        // Then, rebuild children map from stored relationships
        for stored_child in store.children {
            let parent_bytes = hex::decode(&stored_child.parent).unwrap_or_else(|_| vec![0u8; 32]);
            let child_bytes = hex::decode(&stored_child.child).unwrap_or_else(|_| vec![0u8; 32]);
            let parent: aether_unified::transaction::TransactionId = parent_bytes.try_into().unwrap_or_else(|_| [0u8; 32]);
            let child: aether_unified::transaction::TransactionId = child_bytes.try_into().unwrap_or_else(|_| [0u8; 32]);

            // Don't skip GENESIS_HASH - it should be in children map so it's not considered a tip
            // Add to children map
            if let Some(children_vec) = dag.children_mut().get_mut(&parent) {
                let mut temp_vec: Vec<aether_unified::transaction::TransactionId> = children_vec.clone();
                let child_to_push: aether_unified::transaction::TransactionId = child;
                temp_vec.push(child_to_push);
                dag.children_mut().insert(parent, temp_vec);
            } else {
                let child_vec: Vec<aether_unified::transaction::TransactionId> = vec![child];
                dag.children_mut().insert(parent, child_vec);
            }
        }

        tracing::info!("  DAG reconstruction complete");
        tracing::info!("  Ledger (balances+nonces) loaded from Sled as source of truth");

        // Verify ledger balances are consistent with DAG (warning only, don't auto-fix)
        let ledger_balances = ledger.get_all_balances();
        tracing::info!("  Ledger has {} accounts with balances", ledger_balances.len());
        tracing::info!("  Ledger has {} accounts with nonces", ledger.nonces.len());

        (dag, consensus, ledger.balances.clone(), orphans, missing_parent_hashes)
    } else {
        tracing::info!("🌱 No existing DAG state found, initializing genesis...");
        let genesis_config = GenesisConfig::default();
        let (dag, consensus, balances, orphans, missing_parent_hashes) = initialize_genesis(genesis_config);
        
        // Sync genesis balances to ledger
        for (addr_hex, balance) in &balances {
            let addr_bytes = hex::decode(addr_hex)?;
            let address: Address = addr_bytes.as_slice().try_into()
                .map_err(|e| format!("Invalid address length: {}", e))?;
            ledger.set_balance(&address, *balance);
        }
        ledger.save().await?;
        
        (dag, consensus, ledger.balances.clone(), orphans, missing_parent_hashes)
    };

    let orphans: Arc<RwLock<std::collections::HashMap<[u8; 32], aether_unified::transaction::Transaction>>> = Arc::new(RwLock::new(orphans));
    let missing_parent_hashes: Vec<Vec<u8>> = missing_parent_hashes;

    let dag: Arc<RwLock<DAG>> = Arc::new(RwLock::new(dag));
    let consensus: Arc<RwLock<VQVConsensus>> = Arc::new(RwLock::new(consensus));
    let _balances: Arc<RwLock<std::collections::HashMap<String, u64>>> = Arc::new(RwLock::new(balances));
    
    // Wrap ledger in Arc<RwLock> for shared access
    let ledger: Arc<RwLock<aether_unified::ledger::Ledger>> = Arc::new(RwLock::new(ledger));

    // Spawn dedicated save worker task with balance count tracking
    let ledger_for_save = ledger.clone();
    tokio::spawn(async move {
        while let Some(event) = save_rx.recv().await {
            match event {
                aether_unified::SyncEvent::SaveRequested => {
                    let ledger = ledger_for_save.read().await;
                    if let Err(e) = ledger.save().await {
                        tracing::error!("Failed to save ledger: {}", e);
                    } else {
                        tracing::debug!("💾 Ledger saved to Sled");
                    }
                    drop(ledger);
                }
            }
        }
    });
    
    tracing::info!("💾 DAG loaded from JSON");
    tracing::info!("🔄 Rebuilding tips...");
    {
        let mut dag_write: tokio::sync::RwLockWriteGuard<'_, DAG> = dag.write().await;
        dag_write.rebuild_tips();
        tracing::info!("  TIP COUNT: {}", dag_write.transaction_count());
    }
    tracing::info!("✅ Tips rebuilt");

    // STRATEGY SUMMARY:
    // - DAG: Persisted to JSON, reconstructed at boot (temporal structure only)
    // - Ledger (balances+nonces): Persisted to Sled, loaded as source of truth
    // - Mempool: Not persisted, lost on restart (acceptable - transactions can be resubmitted)
    // - Orphans: Not persisted, lost on restart (acceptable - will be re-detected via P2P)
    // - Nonces: Persisted in ledger, validated against last_nonce at boot

    // Initialize storage (temporarily disabled for Windows build)
    // let rocksdb_path = data_dir.join("rocksdb");
    // let storage = Storage::open(&rocksdb_path)?;
    // tracing::info!("✓ Storage initialized at {:?}", rocksdb_path);

    tracing::info!("⚙️  Initializing Micro-PoW...");

    // Initialize Micro-PoW with adaptive difficulty
    let difficulty_adjuster = DifficultyAdjuster::default();
    let pow = MicroPoW::default();
    tracing::info!("✓ Micro-PoW initialized");
    tracing::info!("  Initial Difficulty: {}", pow.difficulty().value());
    tracing::info!("  Target TPS: {}", difficulty_adjuster.target_tps());

    // Initialize mempool
    let mempool = Arc::new(RwLock::new(Mempool::new(1000, 10)));
    tracing::info!("✓ Mempool initialized");

    tracing::info!("📡 Creating P2P channel...");

    // Create mpsc channel for P2P transactions
    let (tx_channel, mut tx_receiver) = tokio::sync::mpsc::unbounded_channel::<Transaction>();
    tracing::info!("✓ P2P channel created");

    let p2p_dag_for_tips = dag.clone();
    let get_tips: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync> = Arc::new(move || {
        // Use try_read for non-blocking access
        if let Ok(dag_lock) = p2p_dag_for_tips.try_read() {
            let dag_ref: &DAG = &*dag_lock;
            // Use the new cumulative weight-based selector
            dag_ref.get_tips_with_selector()
                .iter()
                .map(|id| id.to_vec())
                .collect()
        } else {
            Vec::new()
        }
    });

    let p2p_dag_for_hashes = dag.clone();
    let get_dag_hashes: Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync> = Arc::new(move || {
        if let Ok(dag_lock) = p2p_dag_for_hashes.try_read() {
            dag_lock.transactions().keys().map(|k| k.to_vec()).collect()
        } else {
            Vec::new()
        }
    });

    let p2p_dag_for_tx = dag.clone();
    let get_transaction_by_hash: Arc<dyn Fn(&[u8]) -> Option<Transaction> + Send + Sync> = Arc::new(move |hash| {
        // Use try_read for non-blocking access
        if let Ok(dag_lock) = p2p_dag_for_tx.try_read() {
            let dag_ref: &DAG = &*dag_lock;
            let tx_id: aether_unified::transaction::TransactionId = hash.try_into().ok()?;
            dag_ref.transactions().get(&tx_id).cloned()
        } else {
            None
        }
    });

    let p2p_config = P2PConfig {
        listen_addr: format!("0.0.0.0:{}", p2p_port).parse()
            .map_err(|e| format!("Failed to parse P2P listen address: {}", e))?,
        bootnodes,
    };

    let p2p_ledger = ledger.clone();
    let get_balance: Arc<dyn Fn(&[u8; 32]) -> u64 + Send + Sync> = Arc::new(move |addr| {
        if let Ok(ledger_lock) = p2p_ledger.try_read() {
            ledger_lock.get_balance(addr)
        } else {
            0
        }
    });

    let p2p_dag = dag.clone();
    let p2p_dag_store_path = dag_store_path.clone();
    let save_dag = Arc::new(move || {
        // JSON writes removed - Sled is now the primary storage
    });

    let process_orphans_dag = dag.clone();
    let process_orphans_ledger = ledger.clone();
    let process_orphans_orphans = orphans.clone();
    let process_orphans = Arc::new(move || {
        let dag = process_orphans_dag.clone();
        let ledger = process_orphans_ledger.clone();
        let orphans = process_orphans_orphans.clone();
        tokio::spawn(async move {
            // Check if any orphans can now be processed
            let mut orphans_to_remove = Vec::new();
            {
                let orphans_lock = orphans.read().await;
                let dag_lock: tokio::sync::RwLockReadGuard<'_, DAG> = dag.read().await;

                for (tx_id, orphan) in orphans_lock.iter() {
                    let parent0_ok = orphan.parents[0] == [0u8; 32] || dag_lock.transactions().contains_key(&orphan.parents[0]);
                    let parent1_ok = orphan.parents[1] == [0u8; 32] || dag_lock.transactions().contains_key(&orphan.parents[1]);

                    if parent0_ok && parent1_ok {
                        tracing::info!("🔗 [Sync] Transaction {} resolved - parents now available via P2P sync", hex::encode(&tx_id[..8]));
                        orphans_to_remove.push(*tx_id);
                    }
                }
            }

            // Process resolved orphans
            if !orphans_to_remove.is_empty() {
                let mut orphans_lock = orphans.write().await;
                let mut dag_lock: tokio::sync::RwLockWriteGuard<'_, DAG> = dag.write().await;
                let mut ledger_lock = ledger.write().await;

                for tx_id in orphans_to_remove {
                    if let Some(orphan) = orphans_lock.remove(&tx_id) {
                        // Orphan is pre-validated when stored, use validated method for zero-trust
                        dag_lock.add_transaction_validated(orphan.clone()).expect("Orphan should be valid");

                        // Update ledger
                        let sender_hex = hex::encode(orphan.sender);
                        let receiver_hex = hex::encode(orphan.receiver);
                        let amount = orphan.amount;

                        if let Some(balance) = ledger_lock.balances.get_mut(&sender_hex) {
                            *balance = balance.saturating_sub(amount);
                        }
                        if let Some(balance) = ledger_lock.balances.get_mut(&receiver_hex) {
                            *balance = balance.saturating_add(amount);
                        }
                    }
                }

                // Rebuild tips after processing orphans
                dag_lock.rebuild_tips();
                tracing::info!("✅ Orphans resolved, tips rebuilt");
            }
        });
    });

    tracing::info!("🌐 Initializing P2P network...");

    // Initialize P2P network
    let p2p_network = Arc::new(P2PNetwork::new(p2p_config, tx_channel, get_dag_hashes, get_transaction_by_hash, get_balance, save_dag, process_orphans, get_tips));
    tracing::info!("✅ P2P network initialized");
    
    let p2p_network_clone = p2p_network.clone();
    tracing::info!("🔄 Spawning P2P Network task...");
    tokio::spawn(async move {
        tracing::info!("✅ P2P Network task spawned");
        if let Err(e) = p2p_network_clone.start().await {
            tracing::error!("P2P network error: {}", e);
        }
    });

    tracing::info!("🔄 Spawning P2P transaction receiver...");
    // Spawn task to process transactions from P2P network
    let dag_for_p2p = dag.clone();
    let mempool_for_p2p = mempool.clone();
    let ledger_for_p2p = ledger.clone();
    let consensus_for_p2p = consensus.clone();
    let p2p_network_for_p2p = p2p_network.clone();
    let save_tx_for_p2p = save_tx.clone();
    let miner_addr_for_p2p = miner_address.clone();
    let ledger_path_for_p2p = ledger_path.clone();
    let storage_for_p2p = storage.clone();
    let orphans_for_p2p = orphans.clone();
    
    tokio::spawn(async move {
        tracing::info!("✅ P2P transaction receiver task spawned");
        while let Some(tx) = tx_receiver.recv().await {
            // Create a minimal AetherRpcImpl for validation
            let rpc_impl = aether_unified::rpc::AetherRpcImpl::new(
                consensus_for_p2p.clone(),
                dag_for_p2p.clone(),
                ledger_for_p2p.clone(),
                storage_for_p2p.clone(),
                ledger_path_for_p2p.clone(),
                mempool_for_p2p.clone(),
                p2p_network_for_p2p.clone(),
                save_tx_for_p2p.clone(),
                Arc::new(RwLock::new(true)),
                miner_addr_for_p2p.clone(),
                orphans_for_p2p.clone(),
            );
            
            // Use the same validation and processing logic as RPC
            match rpc_impl.process_transaction(tx, "P2P").await {
                Ok(_) => {
                    tracing::info!("✅ P2P transaction accepted and processed");
                    // Try to process orphans after each successful transaction
                    rpc_impl.process_orphans().await;
                }
                Err(e) => {
                    tracing::warn!("❌ P2P transaction rejected: {}", e);
                }
            }
        }
    });

    tracing::info!("✓ P2P Network initialized");
    tracing::info!("  Listening on port {}", p2p_port);

    // Request missing parents for orphans via P2P
    if !missing_parent_hashes.is_empty() {
        let p2p_for_orphans = p2p_network.clone();
        tokio::spawn(async move {
            // Wait a bit for P2P connections to establish
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            for parent_hash in missing_parent_hashes {
                tracing::info!("📡 Orphan Solver - Requesting missing parent via P2P: {}", hex::encode(&parent_hash));
                p2p_for_orphans.request_transaction(parent_hash).await;
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        });
    }

    // Create shared mining state
    let mining_enabled = Arc::new(RwLock::new(true));

    // Start periodic orphan processing task
    let orphans_periodic = orphans.clone();
    let dag_periodic = dag.clone();
    let ledger_periodic = ledger.clone();
    let storage_periodic = storage.clone();
    let ledger_path_periodic = ledger_path.clone();
    let mempool_periodic = mempool.clone();
    let consensus_periodic = consensus.clone();
    let p2p_periodic = p2p_network.clone();
    let save_tx_periodic = save_tx.clone();
    let mining_enabled_periodic = mining_enabled.clone();
    let miner_addr_periodic = miner_address.clone();
    
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            
            let rpc_impl = aether_unified::rpc::AetherRpcImpl::new(
                consensus_periodic.clone(),
                dag_periodic.clone(),
                ledger_periodic.clone(),
                storage_periodic.clone(),
                ledger_path_periodic.clone(),
                mempool_periodic.clone(),
                p2p_periodic.clone(),
                save_tx_periodic.clone(),
                mining_enabled_periodic.clone(),
                miner_addr_periodic.clone(),
                orphans_periodic.clone(),
            );
            
            rpc_impl.process_orphans().await;
        }
    });

    tracing::info!("📡 About to start RPC server...");

    // Start RPC server in background
    let rpc_addr: SocketAddr = format!("0.0.0.0:{}", rpc_port).parse()?;
    let rpc_dag = dag.clone();
    let rpc_consensus = consensus.clone();
    let rpc_ledger = ledger.clone();
    let rpc_ledger_path = ledger_path.clone();
    let rpc_mempool = mempool.clone();
    let rpc_p2p = p2p_network.clone();
    let rpc_save_tx = save_tx.clone();
    let rpc_storage = storage.clone();
    let mining_enabled_rpc = mining_enabled.clone();
    let rpc_orphans = orphans.clone();

    tracing::info!("🔄 Spawning RPC Server task...");
    tokio::spawn(async move {
        tracing::info!("✅ RPC Server task spawned");
        if let Err(e) = start_rpc_server(rpc_addr, rpc_consensus, rpc_dag, rpc_ledger, rpc_storage, rpc_ledger_path, rpc_mempool, rpc_p2p, rpc_save_tx, mining_enabled_rpc, miner_address, rpc_orphans).await {
            tracing::error!("RPC server error: {}", e);
        }
    });
    tracing::info!("✓ RPC Server initialized");

    tracing::info!("✅ Aether Node Ready");

    // Start based on node type
    match node_type.as_str() {
        "miner" => {
            tracing::info!("⛏️  Starting Mining Mode...");
            tracing::info!("  Node will actively mine transactions to secure the DAG");

            // Clone mempool for mining task
            let mining_mempool = mempool.clone();
            let dag_store_path_clone = dag_store_path.clone();
            let _data_dir_clone = data_dir.clone();
            let mining_enabled_clone = mining_enabled.clone();

            // Spawn mining task in isolation
            tracing::info!("🔄 Spawning Mining task...");
            tokio::spawn(async move {
                tracing::info!("✅ Mining task spawned");
                let mut iteration = 0u64;
                let _save_tx_clone = save_tx.clone();
                let mut last_status_time = std::time::Instant::now();

                // Track real-time TPS
                let mut transactions_processed = 0u64;
                let start_time = std::time::Instant::now();

                // Mining loop - Reactive: only works when mempool has transactions
                loop {
                    // Check if mining is enabled
                    let should_mine = *mining_enabled_clone.read().await;
                    if !should_mine {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        continue;
                    }
                    if let Some(tx) = {
                        let mut mempool_lock: tokio::sync::RwLockWriteGuard<'_, Mempool> = mining_mempool.write().await;
                        mempool_lock.pop_front()
                    } {
                        tracing::info!("📦 Transaction found in mempool! Mining...");
                        {
                            let mut dag_write: tokio::sync::RwLockWriteGuard<'_, DAG> = dag.write().await;
                            // Mempool transaction is pre-validated, use validated method for zero-trust
                            dag_write.add_transaction_validated(tx).expect("Mempool transaction should be valid");
                        }
                        // Ici, calcule le VRAI TPS
                        transactions_processed += 1;
                        let elapsed_secs = start_time.elapsed().as_secs_f64();
                        let real_tps = if elapsed_secs > 0.0 {
                            transactions_processed as f64 / elapsed_secs
                        } else {
                            0.0
                        };
                        let dag_read: tokio::sync::RwLockReadGuard<'_, DAG> = dag.read().await;
                        tracing::info!("🔄 Mining iteration {} | TPS: {:.2} | Transactions: {}",
                            iteration, real_tps, dag_read.transaction_count());
                        iteration += 1;

                        // JSON writes removed - Sled is now the primary storage
                        // let dag_clone = dag.clone();
                        // let dag_store_path_clone = dag_store_path_clone.clone();
                        // tokio::spawn(async move {
                        //     let dag_lock = dag_clone.read().await;
                        //     if let Err(e) = save_dag_to_json(&*dag_lock, &dag_store_path_clone).await {
                        //         tracing::error!("Failed to save DAG: {}", e);
                        //     }
                        // });
                    } else {
                        // Afficher le statut périodiquement
                        if last_status_time.elapsed() >= std::time::Duration::from_secs(5) {
                            let mempool_size = mining_mempool.read().await.size();
                            let dag_read: tokio::sync::RwLockReadGuard<'_, DAG> = dag.read().await;
                            let elapsed_secs = start_time.elapsed().as_secs_f64();
                            let real_tps = if elapsed_secs > 0.0 {
                                transactions_processed as f64 / elapsed_secs
                            } else {
                                0.0
                            };
                            tracing::info!("⛏️  Mining Active | Mempool: {} tx | DAG: {} tx | TPS: {:.2} | Connected to RPC",
                                mempool_size, dag_read.transaction_count(), real_tps);
                            last_status_time = std::time::Instant::now();
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    }
                }
            });
            tracing::info!("✓ Mining task initialized");
        }
        "validator" => {
            tracing::info!("🔒 Starting Validator Mode...");
            tracing::info!("  Node will participate in VQV consensus");

            // Validator loop
            tracing::info!("🔄 Spawning Validator task...");
            tokio::spawn(async move {
                tracing::info!("✅ Validator task spawned");
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });
            tracing::info!("✓ Validator task initialized");
        }
        "observer" => {
            tracing::info!("👁️  Starting Observer Mode...");
            tracing::info!("  Node will monitor the network without participating in consensus");

            // Observer loop
            tracing::info!("🔄 Spawning Observer task...");
            tokio::spawn(async move {
                tracing::info!("✅ Observer task spawned");
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });
            tracing::info!("✓ Observer task initialized");
        }
        _ => {
            tracing::error!("❌ Unknown node type: {}", node_type);
            tracing::error!("Valid types: miner, validator, observer");
            std::process::exit(1);
        }
    }

    // Keep-alive: wait for Ctrl+C signal
    tracing::info!("✅ Node running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await.map_err(|e| format!("Failed to listen for Ctrl+C: {}", e))?;
    tracing::info!("🛑 Shutting down gracefully...");

    Ok(())
}

/// Lightweight transaction client for sending transactions via RPC
async fn send_transaction_client(
    wallet_path: &str,
    receiver_hex: &str,
    amount: u64,
    fee: u64,
    rpc_url: &str,
    password: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load wallet (with password if provided)
    let wallet: aether_unified::wallet::Wallet = match Wallet::from_file(wallet_path, password).await {
        Ok(w) => w,
        Err(e) if e.to_string().contains("Password required") => {
            eprintln!("⚠️  Warning: Wallet is encrypted but no password provided");
            eprintln!("  Use --password option to unlock the wallet");
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    let sender_address = wallet.address(); // Already returns first 32 bytes of public_key

    // Decode receiver address
    let receiver_address: Address = hex::decode(receiver_hex)?
        .try_into()
        .map_err(|_| "Invalid receiver address hex")?;

    // Fetch account_nonce from RPC
    let client = reqwest::Client::new();
    let rpc_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "aether_getAccountNonce",
        "params": [hex::encode(sender_address)],
        "id": 1
    });

    let account_nonce = match client
        .post(rpc_url)
        .json(&rpc_payload)
        .send()
        .await
    {
        Ok(response) => {
            match response.json::<serde_json::Value>().await {
                Ok(json) => {
                    json.get("result")
                        .and_then(|r| r.get("next_nonce"))
                        .and_then(|n| n.as_u64())
                        .ok_or_else(|| "Failed to extract next_nonce from RPC response")?
                }
                Err(e) => {
                    return Err(format!("Failed to parse account_nonce response: {}", e).into());
                }
            }
        }
        Err(e) => {
            return Err(format!("Failed to fetch account_nonce from RPC: {}", e).into());
        }
    };

    // Create transaction (using genesis as parents for simplicity)
    // Sender is already the first 32 bytes of public_key (matches GUI logic)
    let tx = Transaction::new(
        [[0u8; 32]; 2], // Genesis parents
        sender_address,
        receiver_address,
        amount,
        fee,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64,
        0, // Initial PoW nonce (will be mined)
        account_nonce, // Fetched from RPC
        vec![0u8; 64], // Will be signed
        wallet.public_key_bytes(),
    );

    // Mine nonce locally first
    let difficulty = Transaction::default_difficulty();
    println!("{}", "⛏️  Mining transaction...".yellow());
    let start = std::time::Instant::now();
    let nonce = tx.mine_nonce(difficulty);
    let elapsed = start.elapsed();
    println!("{} Nonce: {} (took {:.2}s)", "✓ Mined".green(), nonce, elapsed.as_secs_f64());
    
    // Update transaction with mined nonce
    let mut signed_tx = tx.clone();
    signed_tx.nonce = nonce;
    
    // Re-compute hash with mined nonce (this will be the final tx.id)
    signed_tx.id = signed_tx.compute_hash();

    // Sign transaction with the signing hash (excludes signature and public_key)
    let _signing_hash = signed_tx.compute_signing_hash();
    let signature = wallet.sign_transaction(&signed_tx)?;
    signed_tx.signature = signature.clone();

    // Prepare RPC request
    let client = reqwest::Client::new();
    let rpc_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "aether_sendTransaction",
        "params": [hex::encode(bincode::serialize(&signed_tx)?)],
        "id": 1
    });

    // Send to RPC server
    let response = match client
        .post(rpc_url)
        .json(&rpc_payload)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("{}", "⚠️  Failed to connect to RPC server".yellow());
            eprintln!("  URL: {}", rpc_url.cyan());
            eprintln!("  Error: {}", e);
            eprintln!("  Make sure the node is running with RPC enabled");
            std::process::exit(1);
        }
    };

    if response.status().is_success() {
        println!("{}", "✓ Transaction sent & signed!".green());
        println!("  Sender: {}", wallet.address_string().cyan());
        println!("  Receiver: {}", receiver_hex.cyan());
        println!("  Amount: {}", amount.to_string().cyan());
        println!("  Fee: {}", fee.to_string().cyan());
        println!("  Nonce: {}", nonce.to_string().cyan());
        println!("  Signature: {}", hex::encode(&signature).cyan());
    } else {
        let error_text = response.text().await?;
        eprintln!("{}", format!("✗ Failed to send transaction: {}", error_text).red());
        std::process::exit(1);
    }

    Ok(())
}

/// Balance client for querying balance via RPC
async fn balance_client(address_hex: &str, rpc_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Prepare RPC request
    let client = reqwest::Client::new();
    let rpc_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "aether_getBalance",
        "params": [address_hex],
        "id": 1
    });

    // Send to RPC server
    let response = match client
        .post(rpc_url)
        .json(&rpc_payload)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("{}", "⚠️  Failed to connect to RPC server".yellow());
            eprintln!("  URL: {}", rpc_url.cyan());
            eprintln!("  Error: {}", e);
            eprintln!("  Make sure the node is running with RPC enabled");
            std::process::exit(1);
        }
    };

    if response.status().is_success() {
        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(result) = response_json.get("result") {
            let balance = result.get("balance").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("{}", "✓ Balance retrieved".green());
            println!("  Address: {}", address_hex.cyan());
            println!("  Balance: {} AETH", balance.to_string().cyan());
        } else if let Some(error) = response_json.get("error") {
            eprintln!("{}", format!("✗ RPC error: {}", error).red());
            std::process::exit(1);
        }
    } else {
        let error_text = response.text().await?;
        eprintln!("{}", format!("✗ Failed to get balance: {}", error_text).red());
        std::process::exit(1);
    }

    Ok(())
}
