use eframe::egui;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use aether_unified::wallet::Wallet;
use aether_unified::transaction::Transaction;
use ed25519_dalek::SigningKey;
use hex;

#[derive(Serialize, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    method: String,
    params: serde_json::Value,
    id: u32,
}

#[derive(Serialize, Deserialize)]
struct RpcResponse {
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

#[derive(Clone)]
struct RpcClient {
    client: Client,
    url: String,
}

impl RpcClient {
    fn new(url: String) -> Self {
        Self {
            client: Client::new(),
            url,
        }
    }

    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: 1,
        };

        let response = self
            .client
            .post(&self.url)
            .json(&request)
            .send()
            .map_err(|e| format!("Failed to send request: {}", e))?;

        let rpc_response: RpcResponse = response
            .json()
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        if let Some(error) = rpc_response.error {
            return Err(format!("RPC Error: {}", error));
        }

        rpc_response.result.ok_or_else(|| "No result".to_string())
    }
}

struct AetherGui {
    rpc_client: RpcClient,
    wallet: Option<Wallet>,
    address: String,
    private_key: String,
    balance_result: String,
    faucet_result: String,
    dag_stats_result: String,
    mining_status_result: String,
    tx_result: String,
    recipient: String,
    amount: String,
    tx_hex: String,
    connected: bool,
    show_private_key: bool,
}

impl AetherGui {
    fn new() -> Self {
        Self {
            rpc_client: RpcClient::new("http://localhost:9933".to_string()),
            wallet: None,
            address: String::new(),
            private_key: String::new(),
            balance_result: String::new(),
            faucet_result: String::new(),
            dag_stats_result: String::new(),
            mining_status_result: String::new(),
            tx_result: String::new(),
            recipient: String::new(),
            amount: String::new(),
            tx_hex: String::new(),
            connected: false,
            show_private_key: false,
        }
    }
}

impl eframe::App for AetherGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("🚀 AETHER SEDC GUI");
            ui.separator();
            
            // Check connection status
            if ui.button("Check Connection").clicked() {
                let client = self.rpc_client.clone();
                match client.call("aether_getMiningStatus", serde_json::json!([])) {
                    Ok(_) => self.connected = true,
                    Err(_) => self.connected = false,
                }
            }

            ui.label(if self.connected {
                "Status: ✅ Connected"
            } else {
                "Status: ❌ Disconnected"
            });
            
            ui.separator();

            // Wallet Section
            ui.heading("💰 Wallet");
            
            if ui.button("Create New Wallet").clicked() {
                let wallet = Wallet::new();
                self.wallet = Some(wallet.clone());
                self.address = hex::encode(wallet.address());
                self.private_key = wallet.secret_key_hex.clone();
                self.balance_result = "Wallet created! Save your private key!".to_string();
            }
            
            if ui.button("Load from Private Key").clicked() {
                if !self.private_key.is_empty() {
                    match hex::decode(&self.private_key) {
                        Ok(key_bytes) if key_bytes.len() == 32 => {
                            let mut secret_key_bytes = [0u8; 32];
                            secret_key_bytes.copy_from_slice(&key_bytes);
                            let signing_key = SigningKey::from_bytes(&secret_key_bytes);
                            let verifying_key = signing_key.verifying_key();
                            let wallet = Wallet {
                                public_key_hex: hex::encode(verifying_key.to_bytes()),
                                secret_key_hex: self.private_key.clone(),
                                mnemonic: None,
                            };
                            self.wallet = Some(wallet.clone());
                            self.address = hex::encode(wallet.address());
                            self.balance_result = "Wallet loaded successfully!".to_string();
                        }
                        _ => {
                            self.balance_result = "Invalid private key".to_string();
                        }
                    }
                }
            }
            
            ui.separator();
            
            ui.horizontal(|ui| {
                ui.label("Address:");
                ui.text_edit_singleline(&mut self.address);
            });
            
            ui.horizontal(|ui| {
                ui.label("Private Key:");
                if self.show_private_key {
                    ui.text_edit_singleline(&mut self.private_key);
                } else {
                    ui.label("••••••••••••••••••••••••••••••••");
                }
                if ui.button(if self.show_private_key { "Hide" } else { "Show" }).clicked() {
                    self.show_private_key = !self.show_private_key;
                }
            });
            
            if ui.button("Get Balance").clicked() {
                let client = self.rpc_client.clone();
                let address = self.address.clone();
                match client.call("aether_getBalance", serde_json::json!([address])) {
                    Ok(r) => self.balance_result = serde_json::to_string_pretty(&r).unwrap_or_else(|_| "Error parsing".to_string()),
                    Err(e) => self.balance_result = format!("Error: {}", e),
                }
            }
            
            if ui.button("Use Faucet").clicked() {
                let client = self.rpc_client.clone();
                let address = self.address.clone();
                match client.call("aether_faucet", serde_json::json!([address])) {
                    Ok(r) => self.faucet_result = serde_json::to_string_pretty(&r).unwrap_or_else(|_| "Error parsing".to_string()),
                    Err(e) => self.faucet_result = format!("Error: {}", e),
                }
            }

            ui.separator();
            
            // Network Stats
            ui.heading("📊 Network Stats");
            if ui.button("Get DAG Stats").clicked() {
                let client = self.rpc_client.clone();
                match client.call("aether_getDagStats", serde_json::json!([])) {
                    Ok(r) => self.dag_stats_result = serde_json::to_string_pretty(&r).unwrap_or_else(|_| "Error parsing".to_string()),
                    Err(e) => self.dag_stats_result = format!("Error: {}", e),
                }
            }
            
            if ui.button("Get Mining Status").clicked() {
                let client = self.rpc_client.clone();
                match client.call("aether_getMiningStatus", serde_json::json!([])) {
                    Ok(r) => self.mining_status_result = serde_json::to_string_pretty(&r).unwrap_or_else(|_| "Error parsing".to_string()),
                    Err(e) => self.mining_status_result = format!("Error: {}", e),
                }
            }

            ui.separator();

            // Send Transaction
            ui.heading("📤 Send Transaction");
            ui.horizontal(|ui| {
                ui.label("Recipient:");
                ui.text_edit_singleline(&mut self.recipient);
            });
            ui.horizontal(|ui| {
                ui.label("Amount (AETH):");
                ui.text_edit_singleline(&mut self.amount);
            });
            
            if ui.button("Create & Sign Transaction").clicked() {
                if let Some(ref wallet) = self.wallet {
                    if !self.recipient.is_empty() && !self.amount.is_empty() {
                        let amount: u64 = self.amount.parse().unwrap_or(0);
                        let recipient: [u8; 32] = match hex::decode(&self.recipient) {
                            Ok(bytes) if bytes.len() == 32 => {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&bytes);
                                arr
                            }
                            _ => {
                                self.tx_result = "Invalid recipient address".to_string();
                                return;
                            }
                        };
                        
                        let sender: [u8; 32] = match hex::decode(&self.address) {
                            Ok(bytes) if bytes.len() == 32 => {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&bytes);
                                arr
                            }
                            _ => {
                                self.tx_result = "Invalid sender address".to_string();
                                return;
                            }
                        };
                        
                        // Fetch tips from RPC for proper parent selection
                        let parents = match self.rpc_client.call("aether_getTips", serde_json::json!([])) {
                            Ok(response) => {
                                if let Some(tips_data) = response.get("tips") {
                                    if let Some(tips_array) = tips_data.as_array() {
                                        let mut parent_ids = [[0u8; 32]; 2];
                                        for (i, tip) in tips_array.iter().take(2).enumerate() {
                                            if let Some(tip_str) = tip.as_str() {
                                                if let Ok(tip_bytes) = hex::decode(tip_str) {
                                                    if tip_bytes.len() == 32 {
                                                        parent_ids[i].copy_from_slice(&tip_bytes);
                                                    }
                                                }
                                            }
                                        }
                                        parent_ids
                                    } else {
                                        [[0u8; 32]; 2] // Fallback to genesis
                                    }
                                } else {
                                    [[0u8; 32]; 2] // Fallback to genesis
                                }
                            }
                            Err(_) => [[0u8; 32]; 2], // Fallback to genesis on RPC error
                        };

                        let tx = Transaction::new(
                            parents,
                            sender,
                            recipient,
                            amount,
                            1, // fee
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                            0,
                            1, // nonce
                            vec![0u8; 64], // signature (will be filled)
                            wallet.public_key_bytes(),
                        );
                        
                        let signed_tx = wallet.sign_transaction(&tx);
                        match signed_tx {
                            Ok(signed) => {
                                // Use bincode to serialize
                                match bincode::serialize(&signed) {
                                    Ok(bytes) => {
                                        self.tx_hex = hex::encode(&bytes);
                                        self.tx_result = "Transaction created and signed!".to_string();
                                    }
                                    Err(e) => {
                                        self.tx_result = format!("Error serializing transaction: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                self.tx_result = format!("Error signing transaction: {}", e);
                            }
                        }
                    } else {
                        self.tx_result = "Please enter recipient and amount".to_string();
                    }
                } else {
                    self.tx_result = "Please create or load a wallet first".to_string();
                }
            }
            
            ui.separator();
            
            ui.horizontal(|ui| {
                ui.label("TX Hex:");
                ui.text_edit_multiline(&mut self.tx_hex);
            });
            
            if ui.button("Send Transaction").clicked() {
                let client = self.rpc_client.clone();
                let tx_hex = self.tx_hex.clone();
                match client.call("aether_sendTransaction", serde_json::json!([tx_hex])) {
                    Ok(r) => self.tx_result = serde_json::to_string_pretty(&r).unwrap_or_else(|_| "Error parsing".to_string()),
                    Err(e) => self.tx_result = format!("Error: {}", e),
                }
            }

            ui.separator();

            // Results
            ui.heading("📝 Results");
            if !self.balance_result.is_empty() {
                ui.label(&self.balance_result);
            }
            if !self.faucet_result.is_empty() {
                ui.label(&self.faucet_result);
            }
            if !self.dag_stats_result.is_empty() {
                ui.label(&self.dag_stats_result);
            }
            if !self.mining_status_result.is_empty() {
                ui.label(&self.mining_status_result);
            }
            if !self.tx_result.is_empty() {
                ui.label(&self.tx_result);
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("AETHER SEDC GUI"),
        ..Default::default()
    };

    eframe::run_native(
        "AETHER SEDC GUI",
        options,
        Box::new(|_cc| Box::new(AetherGui::new())),
    )
}
