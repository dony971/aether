//! # Explorer API Module
//!
//! Generates JSON structures for DAG visualization.

use crate::transaction::{Transaction, TransactionId, Address};
use crate::parent_selection::DAG;
use serde::{Deserialize, Serialize};

/// DAG graph structure for visualization
#[derive(Debug, Serialize, Deserialize)]
pub struct DagGraph {
    /// Nodes (transactions)
    pub nodes: Vec<DagNode>,
    
    /// Edges (parent-child relationships)
    pub edges: Vec<DagEdge>,
    
    /// Statistics
    pub stats: DagStatistics,
}

/// DAG node (transaction)
#[derive(Debug, Serialize, Deserialize)]
pub struct DagNode {
    /// Transaction ID
    pub id: TransactionId,
    
    /// Sender address
    pub sender: Address,
    
    /// Receiver address
    pub receiver: Address,
    
    /// Amount
    pub amount: u64,
    
    /// Fee
    pub fee: u64,
    
    /// Timestamp
    pub timestamp: u64,
    
    /// Status
    pub status: String,
    
    /// Weight (for visualization)
    pub weight: f64,
    
    /// Color (based on status or amount)
    pub color: String,
}

/// DAG edge (parent relationship)
#[derive(Debug, Serialize, Deserialize)]
pub struct DagEdge {
    /// Source transaction (parent)
    pub from: TransactionId,
    
    /// Target transaction (child)
    pub to: TransactionId,
    
    /// Edge label
    pub label: String,
}

/// DAG statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct DagStatistics {
    /// Total transactions
    pub total_transactions: usize,
    
    /// Number of tips
    pub tip_count: usize,
    
    /// Average amount
    pub avg_amount: f64,
    
    /// Total amount
    pub total_amount: u64,
    
    /// Timestamp range
    pub timestamp_range: (u64, u64),
}

/// Explorer API
pub struct ExplorerApi {
    dag: DAG,
}

impl ExplorerApi {
    /// Create new explorer API
    pub fn new(dag: DAG) -> Self {
        Self { dag }
    }
    
    /// Get recent transactions as DAG graph
    pub fn get_recent_dag(&self, limit: usize) -> DagGraph {
        let transactions: Vec<&Transaction> = self.dag.transactions().values().collect();
        let limit = limit.min(transactions.len());
        let recent_txs = &transactions[transactions.len().saturating_sub(limit)..];
        
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut total_amount = 0u64;
        let mut min_timestamp = u64::MAX;
        let mut max_timestamp = 0u64;
        
        for tx in recent_txs {
            // Calculate color based on amount
            let color = if tx.amount > 1000 {
                "#e74c3c".to_string() // Red for large amounts
            } else if tx.amount > 100 {
                "#3498db".to_string() // Blue for medium amounts
            } else {
                "#2ecc71".to_string() // Green for small amounts
            };
            
            nodes.push(DagNode {
                id: tx.id,
                sender: tx.sender,
                receiver: tx.receiver,
                amount: tx.amount,
                fee: tx.fee,
                timestamp: tx.timestamp,
                status: "confirmed".to_string(),
                weight: tx.weight,
                color,
            });
            
            // Add edges for parent relationships
            for (i, parent) in tx.parents.iter().enumerate() {
                edges.push(DagEdge {
                    from: *parent,
                    to: tx.id,
                    label: format!("parent_{}", i + 1),
                });
            }
            
            total_amount += tx.amount;
            min_timestamp = min_timestamp.min(tx.timestamp);
            max_timestamp = max_timestamp.max(tx.timestamp);
        }
        
        let tip_count = self.dag.transactions().values()
            .filter(|tx| !self.dag.children().contains_key(&tx.id))
            .count();
        let avg_amount = if !recent_txs.is_empty() {
            total_amount as f64 / recent_txs.len() as f64
        } else {
            0.0
        };
        
        let stats = DagStatistics {
            total_transactions: transactions.len(),
            tip_count,
            avg_amount,
            total_amount,
            timestamp_range: (min_timestamp, max_timestamp),
        };
        
        DagGraph {
            nodes,
            edges,
            stats,
        }
    }
    
    /// Get transaction details with parent chain
    pub fn get_transaction_chain(&self, tx_id: TransactionId) -> Option<TransactionChain> {
        let _tx = self.dag.get_transaction(tx_id)?;
        
        let mut chain = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue = vec![tx_id];
        
        while let Some(current) = queue.pop() {
            if !visited.insert(current) {
                continue;
            }
            
            if let Some(current_tx) = self.dag.get_transaction(current) {
                chain.push(current_tx.clone());
                
                for parent in &current_tx.parents {
                    if self.dag.get_transaction(*parent).is_some() {
                        queue.push(*parent);
                    }
                }
            }
        }

        let depth = chain.len();

        Some(TransactionChain {
            tx_id,
            chain,
            depth,
        })
    }
    
    /// Get tip transactions
    pub fn get_tips(&self) -> Vec<TransactionId> {
        self.dag.transactions().values()
            .filter(|tx| !self.dag.children().contains_key(&tx.id))
            .map(|tx| tx.id)
            .collect()
    }
}

/// Transaction chain (for visualization)
#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionChain {
    /// Root transaction ID
    pub tx_id: TransactionId,
    
    /// Chain of transactions (from root to tips)
    pub chain: Vec<Transaction>,
    
    /// Chain depth
    pub depth: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_explorer_api_creation() {
        let dag = DAG::new();
        let api = ExplorerApi::new(dag);
        
        let graph = api.get_recent_dag(10);
        assert_eq!(graph.nodes.len(), 0);
        assert_eq!(graph.edges.len(), 0);
    }

    #[test]
    fn test_dag_graph_serialization() {
        let graph = DagGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
            stats: DagStatistics {
                total_transactions: 0,
                tip_count: 0,
                avg_amount: 0.0,
                total_amount: 0,
                timestamp_range: (0, 0),
            },
        };
        
        let json = serde_json::to_string(&graph).unwrap();
        assert!(json.contains("nodes"));
        assert!(json.contains("edges"));
        assert!(json.contains("stats"));
    }
}
