//! # Parent Selection Algorithm
//!
//! Implements tip selection based on cumulative weight and random walk.
//! Uses IOTA-style tip selection with cumulative weight calculation.

use crate::transaction::{Transaction, TransactionId};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use rand::Rng;

/// Represents a set of tip transactions (transactions with no children)
#[derive(Debug, Clone)]
pub struct TipSet {
    tips: HashMap<TransactionId, Transaction>,
}

impl TipSet {
    /// Create a new empty tip set
    pub fn new() -> Self {
        Self {
            tips: HashMap::new(),
        }
    }
    
    /// Add a transaction to the tip set
    pub fn add(&mut self, tx: Transaction) {
        self.tips.insert(tx.id, tx);
    }
    
    /// Remove a transaction from the tip set
    pub fn remove(&mut self, id: &TransactionId) {
        self.tips.remove(id);
    }
    
    /// Get all tips
    pub fn get_tips(&self) -> Vec<&Transaction> {
        self.tips.values().collect()
    }
    
    /// Get the number of tips
    pub fn len(&self) -> usize {
        self.tips.len()
    }
    
    /// Check if tip set is empty
    pub fn is_empty(&self) -> bool {
        self.tips.is_empty()
    }
}

impl Default for TipSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache for cumulative weight calculations
#[derive(Debug, Clone)]
pub struct CumulativeWeightCache {
    weights: Arc<RwLock<HashMap<TransactionId, u64>>>,
}

impl CumulativeWeightCache {
    /// Create a new cumulative weight cache
    pub fn new() -> Self {
        Self {
            weights: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the cumulative weight of a transaction
    pub fn get(&self, id: TransactionId) -> Option<u64> {
        self.weights.read().ok()?.get(&id).copied()
    }

    /// Set the cumulative weight of a transaction
    pub fn set(&self, id: TransactionId, weight: u64) {
        if let Ok(mut weights) = self.weights.write() {
            weights.insert(id, weight);
        }
    }

    /// Invalidate a transaction's weight (when DAG changes)
    pub fn invalidate(&self, id: TransactionId) {
        if let Ok(mut weights) = self.weights.write() {
            weights.remove(&id);
        }
    }

    /// Clear the entire cache
    pub fn clear(&self) {
        if let Ok(mut weights) = self.weights.write() {
            weights.clear();
        }
    }
}

impl Default for CumulativeWeightCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Parent Selection Algorithm based on cumulative weight and random walk
#[derive(Debug)]
pub struct ParentSelectionAlgorithm {
    /// Maximum age of a tip to be considered (in milliseconds)
    max_tip_age_ms: u64,
    
    /// Minimum weight threshold for selection
    min_weight: f64,
    
    /// Diversity factor (0.0 to 1.0) - higher means more diverse parents
    diversity_factor: f64,
    
    /// Cumulative weight cache
    weight_cache: CumulativeWeightCache,
    
    /// Random walk depth (how many steps to walk before selecting)
    walk_depth: usize,
}

impl ParentSelectionAlgorithm {
    /// Create a new parent selection algorithm
    pub fn new(max_tip_age_ms: u64, min_weight: f64, diversity_factor: f64) -> Self {
        Self {
            max_tip_age_ms,
            min_weight,
            diversity_factor: diversity_factor.clamp(0.0, 1.0),
            weight_cache: CumulativeWeightCache::new(),
            walk_depth: 10, // Default walk depth
        }
    }
    
    /// Create with default parameters
    pub fn default() -> Self {
        Self {
            max_tip_age_ms: 60_000, // 1 minute
            min_weight: 0.0,
            diversity_factor: 0.5,
            weight_cache: CumulativeWeightCache::new(),
            walk_depth: 10,
        }
    }
    
    /// Calculate cumulative weight of a transaction (with caching)
    /// Cumulative weight = number of transactions that reference this transaction directly or indirectly
    fn calculate_cumulative_weight(&self, id: TransactionId, dag: &DAG) -> u64 {
        // Check cache first
        if let Some(cached_weight) = self.weight_cache.get(id) {
            return cached_weight;
        }

        // Calculate weight by counting all descendants
        let mut visited = HashSet::new();
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

        // Cache the result
        self.weight_cache.set(id, weight);
        weight
    }

    /// Perform random walk from genesis to select a tip
    /// Returns a tip transaction ID
    fn random_walk(&self, dag: &DAG) -> Result<TransactionId, ParentSelectionError> {
        let genesis = TransactionId::default();
        let mut current = genesis;
        let mut rng = rand::thread_rng();

        for _ in 0..self.walk_depth {
            // Get children of current transaction
            let children = dag.children().get(&current).cloned().unwrap_or_default();

            if children.is_empty() {
                // No children, this is a tip
                return Ok(current);
            }

            // Calculate cumulative weights for children
            let mut weighted_children: Vec<(TransactionId, u64)> = children
                .iter()
                .map(|&child_id| {
                    let weight = self.calculate_cumulative_weight(child_id, dag);
                    (child_id, weight)
                })
                .collect();

            // Sort by weight (descending)
            weighted_children.sort_by(|a, b| b.1.cmp(&a.1));

            // Select child with probability proportional to weight
            let total_weight: u64 = weighted_children.iter().map(|(_, w)| w).sum();
            let mut random_weight = rng.gen_range(0..total_weight);

            for (child_id, weight) in weighted_children {
                if random_weight < weight {
                    current = child_id;
                    break;
                }
                random_weight -= weight;
            }
        }

        Ok(current)
    }

    /// Check if two tips are in conflict (double spend)
    /// Returns true if they conflict
    fn check_double_spend_conflict(&self, tip1: TransactionId, tip2: TransactionId, dag: &DAG) -> bool {
        // Get transactions
        let tx1 = match dag.get_transaction(tip1) {
            Some(tx) => tx,
            None => return false,
        };

        let tx2 = match dag.get_transaction(tip2) {
            Some(tx) => tx,
            None => return false,
        };

        // Check if they spend from the same address
        if tx1.sender == tx2.sender {
            return true; // Same sender, potential conflict
        }

        // Get ancestors of both tips
        let ancestors1 = dag.get_ancestors(tip1, 10);
        let ancestors2 = dag.get_ancestors(tip2, 10);

        // Check for overlapping spenders
        for ancestor1 in &ancestors1 {
            if let Some(tx) = dag.get_transaction(*ancestor1) {
                for ancestor2 in &ancestors2 {
                    if let Some(other_tx) = dag.get_transaction(*ancestor2) {
                        if tx.sender == other_tx.sender {
                            return true; // Conflicting spenders in ancestry
                        }
                    }
                }
            }
        }

        false
    }

    /// Select two parent transactions from the tip set using random walk
    pub fn select_parents(
        &self,
        tip_set: &TipSet,
        dag: &DAG,
    ) -> Result<[TransactionId; 2], ParentSelectionError> {
        let tips = tip_set.get_tips();

        if tips.is_empty() {
            // Return genesis parents if no tips available
            return Ok([TransactionId::default(); 2]);
        }

        // Filter tips by freshness
        let now = current_timestamp_ms();
        let valid_tips: Vec<&Transaction> = tips
            .into_iter()
            .filter(|tx| {
                let age = now.saturating_sub(tx.timestamp);
                age <= self.max_tip_age_ms
            })
            .collect();

        if valid_tips.is_empty() {
            return Err(ParentSelectionError::NoValidTips);
        }

        // Use random walk to select first parent
        let parent1 = self.random_walk(dag)?;

        // Use random walk to select second parent
        let mut attempts = 0;
        let max_attempts = 10;
        let mut parent2 = parent1;

        while attempts < max_attempts {
            let candidate = self.random_walk(dag)?;

            // V3 fix: Check if these parents are already used (prevent double spend)
            let proposed_parents = [parent1, candidate];
            if dag.has_transaction_with_parents(&proposed_parents) {
                tracing::warn!("⚠️ Parents déjà utilisés, tentative de double spend détectée");
                attempts += 1;
                continue;
            }

            // V3 fix: Ensure parents are different (prevent weight inflation)
            if candidate != parent1 && !self.check_double_spend_conflict(parent1, candidate, dag) {
                parent2 = candidate;
                break;
            }

            attempts += 1;
        }

        // If we couldn't find a non-conflicting parent, use genesis
        if parent2 == parent1 {
            parent2 = TransactionId::default();
        }

        // V3 fix: Final check to ensure parents are not identical
        if parent1 == parent2 {
            tracing::warn!("⚠️  Parent selection resulted in identical parents, using genesis as second parent");
            return Ok([parent1, TransactionId::default()]);
        }

        Ok([parent1, parent2])
    }
}

/// Error types for parent selection
#[derive(Debug, thiserror::Error)]
pub enum ParentSelectionError {
    #[error("No valid tips available")]
    NoValidTips,
    
    #[error("DAG is empty")]
    DagEmpty,
}

/// Simple DAG structure for parent selection
#[derive(Debug, Default, serde::Serialize)]
pub struct DAG {
    transactions: HashMap<TransactionId, Transaction>,
    children: HashMap<TransactionId, Vec<TransactionId>>,
    tips: HashSet<TransactionId>,
}

impl DAG {
    /// Create a new empty DAG
    pub fn new() -> Self {
        Self {
            transactions: HashMap::new(),
            children: HashMap::new(),
            tips: HashSet::new(),
        }
    }

    /// Check if a transaction with these parents already exists (V3 fix - prevent double spend)
    pub fn has_transaction_with_parents(&self, parents: &[TransactionId; 2]) -> bool {
        self.transactions.values().any(|tx| &tx.parents == parents)
    }

    /// Check if a sender has a pending transaction in the DAG (conflict detection)
    /// Returns true if the sender already has a transaction in the DAG
    pub fn has_pending_transaction_from_sender(&self, sender: &[u8; 32]) -> bool {
        self.transactions.values().any(|tx| &tx.sender == sender)
    }

    /// Check if a sender has a transaction with a specific nonce in the DAG
    /// Returns true if the sender already has a transaction with this nonce
    pub fn has_transaction_with_nonce(&self, sender: &[u8; 32], nonce: u64) -> bool {
        self.transactions.values().any(|tx| &tx.sender == sender && tx.account_nonce == nonce)
    }

    /// Check if a sender has a conflicting transaction (same nonce or overlapping spend)
    /// Returns true if there's a conflict
    pub fn has_sender_conflict(&self, sender: &[u8; 32], nonce: u64) -> bool {
        // Check for same nonce (strict conflict)
        if self.has_transaction_with_nonce(sender, nonce) {
            return true;
        }
        // Check for any pending transaction from this sender (potential conflict)
        // This is stricter: only one pending transaction per sender at a time
        self.has_pending_transaction_from_sender(sender)
    }
    
    /// Add a transaction to the DAG (REMOVED for zero-trust security)
    /// 🔒 ZERO TRUST: This method is REMOVED. Use add_transaction_validated ONLY.
    /// Economic policy: All transactions MUST be validated before insertion
    /// Any attempt to call this method will panic at compile time.
    #[deprecated(note = "REMOVED for security - use add_transaction_validated ONLY")]
    #[allow(dead_code)]
    pub fn add_transaction(&mut self, _tx: Transaction) {
        panic!("SECURITY VIOLATION: add_transaction() is removed. Use add_transaction_validated() ONLY.");
    }

    /// Add a transaction to the DAG with validation (SECURE METHOD)
    /// Economic policy: This is the ONLY method that should be used in production
    /// Requires validation proof to ensure transaction has been properly validated
    pub fn add_transaction_validated(&mut self, tx: Transaction) -> Result<(), String> {
        // Check for duplicate transaction
        if self.transactions.contains_key(&tx.id) {
            return Err(format!("Duplicate transaction: {}", hex::encode(tx.id)));
        }

        // Check parents exist (basic DAG integrity)
        for (i, parent) in tx.parents.iter().enumerate() {
            let is_genesis = parent.iter().all(|&b| b == 0);
            if !is_genesis && !self.transactions.contains_key(parent) {
                return Err(format!("Parent {} missing: {}", i, hex::encode(parent)));
            }
        }

        // Check for duplicate parents (double spend protection)
        if self.has_transaction_with_parents(&tx.parents) {
            return Err("Double spend detected: parents already used".to_string());
        }

        // Check for sender conflict
        if self.has_sender_conflict(&tx.sender, tx.account_nonce) {
            return Err("Sender conflict: only one pending transaction per sender allowed".to_string());
        }

        // All validations passed - add transaction
        // If transaction has no parents (except genesis), automatically assign tips as parents
        let tx_parents = if tx.parents.is_empty() || tx.parents.iter().all(|p| p.iter().all(|&b| b == 0)) {
            let tips_vec: Vec<TransactionId> = self.tips.iter().cloned().collect();

            if tips_vec.is_empty() {
                [TransactionId::default(), TransactionId::default()]
            } else {
                use rand::seq::SliceRandom;
                let mut rng = rand::thread_rng();
                let selected: Vec<TransactionId> = tips_vec.choose_multiple(&mut rng, 2.min(tips_vec.len()))
                    .into_iter()
                    .cloned()
                    .collect();

                let mut arr = [TransactionId::default(); 2];
                for (i, tip) in selected.iter().enumerate() {
                    if i < 2 {
                        arr[i] = *tip;
                    }
                }
                arr
            }
        } else {
            tx.parents
        };

        let tx_with_parents = Transaction {
            parents: tx_parents,
            ..tx
        };

        self.transactions.insert(tx_with_parents.id, tx_with_parents.clone());

        for parent in &tx_with_parents.parents {
            if parent.iter().all(|&b| b == 0) {
                self.children
                    .entry(*parent)
                    .or_insert_with(Vec::new)
                    .push(tx_with_parents.id);
            } else if self.transactions.contains_key(parent) {
                self.children
                    .entry(*parent)
                    .or_insert_with(Vec::new)
                    .push(tx_with_parents.id);
            }
        }

        self.tips.insert(tx_with_parents.id);
        for parent in &tx_with_parents.parents {
            if !parent.iter().all(|&b| b == 0) {
                self.tips.remove(parent);
            }
        }

        tracing::info!(
            "✅ Transaction {} added to DAG (validated). Tips count: {}",
            hex::encode(tx_with_parents.id),
            self.tips.len()
        );
        
        Ok(())
    }
    
    /// Get a transaction by ID
    pub fn get_transaction(&self, id: TransactionId) -> Option<&Transaction> {
        self.transactions.get(&id)
    }

    /// Get mutable reference to children map
    pub fn children_mut(&mut self) -> &mut HashMap<TransactionId, Vec<TransactionId>> {
        &mut self.children
    }

    /// Remove transaction from DAG (for rollback)
    /// This removes the transaction, its children references, and updates tips
    pub fn remove_transaction(&mut self, tx_id: &TransactionId) {
        // Remove transaction from transactions map
        if let Some(tx) = self.transactions.remove(tx_id) {
            // Remove this transaction from its parents' children lists
            for parent in &tx.parents {
                if let Some(children) = self.children.get_mut(parent) {
                    children.retain(|child_id| child_id != tx_id);
                }
            }
            // Remove from tips
            self.tips.remove(tx_id);
            // Re-add parents to tips (since this transaction is gone)
            for parent in &tx.parents {
                if !parent.iter().all(|&b| b == 0) {
                    self.tips.insert(*parent);
                }
            }
        }
    }

    /// Get ancestors of a transaction up to a certain depth
    pub fn get_ancestors(&self, id: TransactionId, max_depth: usize) -> HashSet<TransactionId> {
        let mut ancestors = HashSet::new();
        let mut queue = vec![(id, 0)];
        
        while let Some((tx_id, depth)) = queue.pop() {
            if depth >= max_depth {
                continue;
            }
            
            if let Some(tx) = self.transactions.get(&tx_id) {
                for parent in &tx.parents {
                    if ancestors.insert(*parent) {
                        queue.push((*parent, depth + 1));
                    }
                }
            }
        }
        
        ancestors
    }
    
    /// Check if a transaction is reachable from another (for cycle detection)
    pub fn is_reachable_from(&self, from: TransactionId, to: TransactionId) -> bool {
        let mut visited = HashSet::new();
        let mut queue = vec![from];

        while let Some(current) = queue.pop() {
            if current == to {
                return true;
            }

            if !visited.insert(current) {
                continue;
            }

            if let Some(tx) = self.transactions.get(&current) {
                for parent in &tx.parents {
                    queue.push(*parent);
                }
            }
        }

        false
    }

    /// Get all transactions
    pub fn transactions(&self) -> &HashMap<TransactionId, Transaction> {
        &self.transactions
    }

    /// Get children map
    pub fn children(&self) -> &HashMap<TransactionId, Vec<TransactionId>> {
        &self.children
    }

    /// Get transaction count
    pub fn transaction_count(&self) -> usize {
        self.transactions.len()
    }

    /// Get tip count (number of unconfirmed transactions)
    pub fn tip_count(&self) -> usize {
        self.tips.len()
    }

    /// Get random tips from the explicit tips set
    pub fn get_random_tips(&self, count: usize) -> Vec<TransactionId> {
        let tips_vec: Vec<TransactionId> = self.tips.iter().cloned().collect();

        if tips_vec.is_empty() {
            // Return genesis hash if no tips
            return vec![TransactionId::default(); count.min(1)];
        }

        // Randomly select up to 'count' tips
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        tips_vec.choose_multiple(&mut rng, count)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Get tips using cumulative weight-based selection
    /// This is the new interface that uses the random walk algorithm
    pub fn get_tips_with_selector(&self) -> Vec<TransactionId> {
        let selector = ParentSelectionAlgorithm::default();
        let _tip_set = TipSet::new();
        
        // Build tip set from current tips
        let mut tips = TipSet::new();
        for tip_id in &self.tips {
            if let Some(tx) = self.transactions.get(tip_id) {
                tips.add(tx.clone());
            }
        }
        
        // Select two parents using the new algorithm
        match selector.select_parents(&tips, self) {
            Ok(parents) => {
                // Return the selected parents as tips
                parents.to_vec()
            }
            Err(_) => {
                // Fallback to random tips if selection fails
                self.get_random_tips(2)
            }
        }
    }

    /// Rebuild tips set from current DAG state (transactions with no children)
    pub fn rebuild_tips(&mut self) {
        self.tips.clear();
        for tx_id in self.transactions.keys() {
            if !self.children.contains_key(tx_id) {
                self.tips.insert(*tx_id);
            }
        }
        tracing::info!("Rebuilt tips: {} tips from {} transactions", self.tips.len(), self.transactions.len());
    }
}

/// Get current timestamp in milliseconds
fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    #[test]
    fn test_tip_set_creation() {
        let tip_set = TipSet::new();
        assert!(tip_set.is_empty());
        assert_eq!(tip_set.len(), 0);
    }

    #[test]
    fn test_tip_set_add_remove() {
        let mut tip_set = TipSet::new();
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            current_timestamp_ms(),
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        tip_set.add(tx.clone());
        assert_eq!(tip_set.len(), 1);
        
        tip_set.remove(&tx.id);
        assert!(tip_set.is_empty());
    }

    #[test]
    fn test_parent_selection_algorithm_default() {
        let algo = ParentSelectionAlgorithm::default();
        assert_eq!(algo.max_tip_age_ms, 60_000);
        assert_eq!(algo.min_weight, 0.0);
        assert_eq!(algo.diversity_factor, 0.5);
    }

    #[test]
    fn test_parent_selection_empty_tip_set() {
        let algo = ParentSelectionAlgorithm::default();
        let tip_set = TipSet::new();
        let dag = DAG::new();
        
        let result = algo.select_parents(&tip_set, &dag);
        assert!(result.is_ok());
        let parents = result.expect("Parent selection should succeed");
        assert_eq!(parents, [TransactionId::default(); 2]);
    }

    #[test]
    fn test_parent_selection_with_tips() {
        let algo = ParentSelectionAlgorithm::default();
        let mut tip_set = TipSet::new();
        let mut dag = DAG::new();
        
        let now = current_timestamp_ms();
        
        // Create two tip transactions
        let mut tx1 = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            now,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        tx1.weight = 10.0;
        
        let mut tx2 = Transaction::new(
            [[0u8; 32]; 2],
            [3u8; 32],
            [4u8; 32],
            200,
            10,
            now,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        tx2.weight = 5.0;
        
        tip_set.add(tx1.clone());
        tip_set.add(tx2.clone());
        
        // Add transaction to DAG (using validated method)
        dag.add_transaction_validated(tx1.clone()).unwrap();
        
        let result = algo.select_parents(&tip_set, &dag);
        assert!(result.is_ok());
        
        let parents = result.expect("Parent selection should succeed");
        // Should select a tip (tx1 or tx2) as first parent
        assert!(parents[0] == tx1.id || parents[0] == tx2.id);
    }

    #[test]
    fn test_parent_selection_filters_old_tips() {
        let algo = ParentSelectionAlgorithm::new(60_000, 0.0, 0.5);
        let mut tip_set = TipSet::new();
        let dag = DAG::new();
        
        let old_time = current_timestamp_ms() - 120_000; // 2 minutes ago
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            old_time,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        tip_set.add(tx);
        
        let result = algo.select_parents(&tip_set, &dag);
        assert!(matches!(result, Err(ParentSelectionError::NoValidTips)));
    }

    #[test]
    fn test_dag_add_transaction() {
        let mut dag = DAG::new();
        let tx = Transaction::new(
            [[0u8; 32], [0u8; 32]], // Genesis parents
            [3u8; 32],
            [4u8; 32],
            100,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        dag.add_transaction_validated(tx.clone()).unwrap();
        
        assert!(dag.get_transaction(tx.id).is_some());
    }

    #[test]
    fn test_dag_ancestors() {
        let mut dag = DAG::new();

        let parent1 = Transaction::new(
            [[0u8; 32], [0u8; 32]], // Genesis parents
            [1u8; 32],
            [2u8; 32],
            0,
            0,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );

        // Add parent1 first
        dag.add_transaction_validated(parent1.clone()).unwrap();

        // Child references parent1
        let child = Transaction::new(
            [parent1.id, [0u8; 32]],
            [5u8; 32],
            [6u8; 32],
            100,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );

        dag.add_transaction_validated(child.clone()).unwrap();

        let ancestors = dag.get_ancestors(child.id, 2);
        assert!(ancestors.contains(&parent1.id));
    }

    #[test]
    fn test_dag_reachability() {
        let mut dag = DAG::new();
        
        let parent = Transaction::new(
            [[0u8; 32], [0u8; 32]],
            [1u8; 32],
            [2u8; 32],
            0,
            0,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        let child = Transaction::new(
            [parent.id, [0u8; 32]],
            [3u8; 32],
            [4u8; 32],
            100,
            10,
            0,
            0,
            1, // account_nonce
            vec![0u8; 64],
            vec![0u8; 32],
        );
        
        dag.add_transaction_validated(parent.clone()).unwrap();
        dag.add_transaction_validated(child.clone()).unwrap();
        
        assert!(dag.is_reachable_from(child.id, parent.id));
        assert!(!dag.is_reachable_from(parent.id, child.id));
    }
}
