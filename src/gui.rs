use eframe::egui;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    client: Arc<Client>,
    url: String,
}

impl RpcClient {
    fn new(url: String) -> Self {
        Self {
            client: Arc::new(Client::new()),
            url,
        }
    }

    async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
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
            .await
            .map_err(|e| format!("Failed to send request: {}", e))?;

        let rpc_response: RpcResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        if let Some(error) = rpc_response.error {
            return Err(format!("RPC Error: {}", error));
        }

        rpc_response.result.ok_or_else(|| "No result".to_string())
    }
}

struct AetherGui {
    rpc_client: RpcClient,
    address: String,
    balance_result: String,
    faucet_result: String,
    dag_stats_result: String,
    mining_status_result: String,
    tx_result: String,
    recipient: String,
    amount: String,
    tx_hex: String,
    connected: bool,
}

impl AetherGui {
    fn new() -> Self {
        Self {
            rpc_client: RpcClient::new("http://localhost:9933".to_string()),
            address: "18a0c5f75ce4e0ffe94344cd73ff1ad85b16a2531a29f5d49463b63b8b8e7bf8".to_string(),
            balance_result: String::new(),
            faucet_result: String::new(),
            dag_stats_result: String::new(),
            mining_status_result: String::new(),
            tx_result: String::new(),
            recipient: String::new(),
            amount: String::new(),
            tx_hex: String::new(),
            connected: false,
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
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let _result = client.call("aether_getMiningStatus", serde_json::json!([])).await;
                    ctx.request_repaint();
                });
                self.connected = true;
            }

            ui.label(if self.connected {
                "Status: ✅ Connected"
            } else {
                "Status: ❌ Disconnected"
            });
            
            ui.separator();

            // Wallet Section
            ui.heading("💰 Wallet");
            ui.horizontal(|ui| {
                ui.label("Address:");
                ui.text_edit_singleline(&mut self.address);
            });
            
            if ui.button("Get Balance").clicked() {
                let client = self.rpc_client.clone();
                let address = self.address.clone();
                tokio::spawn(async move {
                    let result = client.call("aether_getBalance", serde_json::json!([address])).await;
                    match result {
                        Ok(r) => println!("Balance: {}", serde_json::to_string_pretty(&r).unwrap()),
                        Err(e) => println!("Error: {}", e),
                    }
                });
                self.balance_result = "Loading...".to_string();
            }
            
            if ui.button("Use Faucet").clicked() {
                let client = self.rpc_client.clone();
                let address = self.address.clone();
                tokio::spawn(async move {
                    let result = client.call("aether_faucet", serde_json::json!([address])).await;
                    match result {
                        Ok(r) => println!("Faucet: {}", serde_json::to_string_pretty(&r).unwrap()),
                        Err(e) => println!("Error: {}", e),
                    }
                });
                self.faucet_result = "Loading...".to_string();
            }

            ui.separator();
            
            // Network Stats
            ui.heading("📊 Network Stats");
            if ui.button("Get DAG Stats").clicked() {
                let client = self.rpc_client.clone();
                tokio::spawn(async move {
                    let result = client.call("aether_getDagStats", serde_json::json!([])).await;
                    match result {
                        Ok(r) => println!("DAG Stats: {}", serde_json::to_string_pretty(&r).unwrap()),
                        Err(e) => println!("Error: {}", e),
                    }
                });
                self.dag_stats_result = "Loading...".to_string();
            }
            
            if ui.button("Get Mining Status").clicked() {
                let client = self.rpc_client.clone();
                tokio::spawn(async move {
                    let result = client.call("aether_getMiningStatus", serde_json::json!([])).await;
                    match result {
                        Ok(r) => println!("Mining Status: {}", serde_json::to_string_pretty(&r).unwrap()),
                        Err(e) => println!("Error: {}", e),
                    }
                });
                self.mining_status_result = "Loading...".to_string();
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
            ui.horizontal(|ui| {
                ui.label("TX Hex:");
                ui.text_edit_singleline(&mut self.tx_hex);
            });
            
            if ui.button("Send Transaction").clicked() {
                let client = self.rpc_client.clone();
                let tx_hex = self.tx_hex.clone();
                tokio::spawn(async move {
                    let result = client.call("aether_sendTransaction", serde_json::json!([tx_hex])).await;
                    match result {
                        Ok(r) => println!("TX Result: {}", serde_json::to_string_pretty(&r).unwrap()),
                        Err(e) => println!("Error: {}", e),
                    }
                });
                self.tx_result = "Loading...".to_string();
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
