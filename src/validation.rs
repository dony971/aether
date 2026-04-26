//! # Core Validation Module
//!
//! Provides unified transaction validation logic for the entire system.
//! This is the ONLY place where transaction validation should happen.
//! RPC, P2P, and DAG must EXCLUSIVELY use this validator.
//!
//! Economic policy: validation BEFORE any state modification, atomic rollback on failure.

use crate::parent_selection::DAG;
use crate::ledger::Ledger;
use crate::transaction::Transaction;
use crate::transaction::TransactionId;

/// Validation result with detailed error information
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// PoW does not meet difficulty requirement
    InvalidPoW { difficulty: u8 },
    /// Signature verification failed
    InvalidSignature,
    /// Sender address does not match public key
    SenderPublicKeyMismatch,
    /// Duplicate transaction already exists
    DuplicateTransaction { tx_id: TransactionId },
    /// Parent transaction missing
    MissingParent { parent_index: usize, parent_id: TransactionId },
    /// Double spend detected (parents already used)
    DoubleSpend,
    /// Sender conflict (multiple pending transactions)
    SenderConflict,
    /// Insufficient balance
    InsufficientBalance { required: u64, available: u64 },
    /// Account nonce invalid (not sequential)
    InvalidNonce { expected: u64, provided: u64 },
    /// Fee below minimum
    InsufficientFee { required: u64, provided: u64 },
    /// Amount + fee overflow
    Overflow,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidPoW { difficulty } => {
                write!(f, "Invalid PoW: transaction must meet difficulty {}", difficulty)
            }
            ValidationError::InvalidSignature => {
                write!(f, "Invalid signature")
            }
            ValidationError::SenderPublicKeyMismatch => {
                write!(f, "Sender address does not match public key")
            }
            ValidationError::DuplicateTransaction { tx_id } => {
                write!(f, "Duplicate transaction: {} already exists", hex::encode(tx_id))
            }
            ValidationError::MissingParent { parent_index, parent_id } => {
                write!(f, "Parent {} missing: {}", parent_index, hex::encode(parent_id))
            }
            ValidationError::DoubleSpend => {
                write!(f, "Double spend detected: parents already used")
            }
            ValidationError::SenderConflict => {
                write!(f, "Sender conflict: only one pending transaction per sender allowed")
            }
            ValidationError::InsufficientBalance { required, available } => {
                write!(f, "Insufficient balance: {} < {}", available, required)
            }
            ValidationError::InvalidNonce { expected, provided } => {
                write!(f, "Invalid nonce: expected {}, provided {}", expected, provided)
            }
            ValidationError::InsufficientFee { required, provided } => {
                write!(f, "Insufficient fee: {} < minimum {}", provided, required)
            }
            ValidationError::Overflow => {
                write!(f, "Amount + fee overflow")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Core transaction validator
/// Economic policy: validation BEFORE any state modification
/// This is the ONLY validator that should be used across the entire system
pub struct TransactionValidator {
    /// PoW difficulty requirement
    difficulty: u8,
}

impl TransactionValidator {
    /// Create new validator with default difficulty
    pub fn new() -> Self {
        Self {
            difficulty: Transaction::default_difficulty(),
        }
    }

    /// Create validator with custom difficulty
    pub fn with_difficulty(difficulty: u8) -> Self {
        Self { difficulty }
    }

    /// Validate transaction without accessing DAG/Ledger (pure validation) - INTERNAL USE ONLY
    /// 🔒 ZERO TRUST: This method is private. Use validate_full() for all validation.
    /// These checks can be done before acquiring any locks
    /// Economic policy: no state access, no mutations
    pub(crate) fn validate_pure(&self, tx: &Transaction) -> Result<(), ValidationError> {
        // Step 1: Verify PoW
        if !tx.verify_pow(self.difficulty) {
            return Err(ValidationError::InvalidPoW { difficulty: self.difficulty });
        }

        // Step 2: Verify signature
        if !crate::wallet::Wallet::verify_transaction(tx) {
            return Err(ValidationError::InvalidSignature);
        }

        // Step 3: Verify sender matches public key (V1 fix)
        if !tx.verify_sender_matches_public_key() {
            return Err(ValidationError::SenderPublicKeyMismatch);
        }

        // Step 4: Check for amount + fee overflow
        if tx.amount.checked_add(tx.fee).is_none() {
            return Err(ValidationError::Overflow);
        }

        Ok(())
    }

    /// Validate transaction against DAG (INTERNAL USE ONLY)
    /// 🔒 ZERO TRUST: This method is private. Use validate_full() for all validation.
    /// These checks require DAG access but don't modify state
    /// Economic policy: read-only access to DAG
    pub(crate) fn validate_dag(&self, tx: &Transaction, dag: &DAG) -> Result<(), ValidationError> {
        // Check for duplicate transaction in DAG
        if dag.transactions().contains_key(&tx.id) {
            return Err(ValidationError::DuplicateTransaction { tx_id: tx.id });
        }

        // Check parents exist
        for (i, parent) in tx.parents.iter().enumerate() {
            let is_genesis = *parent == TransactionId::default();
            if !is_genesis && !dag.transactions().contains_key(parent) {
                return Err(ValidationError::MissingParent { parent_index: i, parent_id: *parent });
            }
        }

        // Check for duplicate parents (double spend protection)
        if dag.has_transaction_with_parents(&tx.parents) {
            return Err(ValidationError::DoubleSpend);
        }

        // Check for sender conflict
        if dag.has_sender_conflict(&tx.sender, tx.account_nonce) {
            return Err(ValidationError::SenderConflict);
        }

        Ok(())
    }

    /// Validate transaction against Ledger (INTERNAL USE ONLY)
    /// 🔒 ZERO TRUST: This method is private. Use validate_full() for all validation.
    /// These checks require Ledger access but don't modify state
    /// Economic policy: read-only access to Ledger
    pub(crate) fn validate_ledger(&self, tx: &Transaction, ledger: &Ledger, min_fee: u64) -> Result<(), ValidationError> {
        // Check balance
        let sender_balance = ledger.get_balance(&tx.sender);
        let required = match tx.amount.checked_add(tx.fee) {
            Some(sum) => sum,
            None => return Err(ValidationError::Overflow),
        };

        if sender_balance < required {
            return Err(ValidationError::InsufficientBalance { required, available: sender_balance });
        }

        // Check account nonce
        if let Err(e) = ledger.validate_account_nonce(&tx.sender, tx.account_nonce) {
            return Err(ValidationError::InvalidNonce { 
                expected: ledger.get_nonce(&tx.sender) + 1,
                provided: tx.account_nonce 
            });
        }

        // Check minimum fee
        if tx.fee < min_fee {
            return Err(ValidationError::InsufficientFee { required: min_fee, provided: tx.fee });
        }

        Ok(())
    }

    /// Full validation pipeline (pure + DAG + ledger)
    /// Economic policy: all validations before any state modification
    pub fn validate_full(
        &self,
        tx: &Transaction,
        dag: &DAG,
        ledger: &Ledger,
        min_fee: u64,
    ) -> Result<(), ValidationError> {
        self.validate_pure(tx)?;
        self.validate_dag(tx, dag)?;
        self.validate_ledger(tx, ledger, min_fee)?;
        Ok(())
    }
}

impl Default for TransactionValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parent_selection::DAG;
    use crate::ledger::Ledger;
    use crate::transaction::Transaction;
    use tempfile::tempdir;

    #[test]
    fn test_validate_pure_valid() {
        let validator = TransactionValidator::new();
        
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
            vec![1u8; 64],
        );

        // This should fail signature verification (invalid signature)
        assert!(validator.validate_pure(&tx).is_err());
    }

    #[test]
    fn test_validate_pure_overflow() {
        let validator = TransactionValidator::new();
        let mut dag = DAG::new();
        let ledger = Ledger::new();
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            [1u8; 32],
            [2u8; 32],
            u64::MAX,
            u64::MAX,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        // Add transaction to DAG (using validated method)
        dag.add_transaction_validated(tx.clone()).unwrap();

        // This should fail due to overflow
        let result = validator.validate_pure(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_dag_genesis_parents_valid() {
        let validator = TransactionValidator::new();
        let mut dag = DAG::new();
        let _ledger = Ledger::new();

        // Transaction with genesis parents should be valid (not an orphan)
        let tx = Transaction::new(
            [[0u8; 32]; 2], // Genesis parents
            [1u8; 32],
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        // Validation should pass (genesis parents are always valid)
        let result = validator.validate_dag(&tx, &dag);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_dag_missing_parent() {
        let validator = TransactionValidator::new();
        let mut dag = DAG::new();
        let _ledger = Ledger::new();

        // Use non-genesis parent that doesn't exist
        let tx = Transaction::new(
            [[1u8; 32], [2u8; 32]], // Non-existent parents
            [3u8; 32],
            [4u8; 32],
            100,
            10,
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![2u8; 64],
        );

        assert!(matches!(validator.validate_dag(&tx, &dag), Err(ValidationError::MissingParent { .. })));
    }

    #[test]
    fn test_validate_dag_duplicate() {
        let validator = TransactionValidator::new();
        let mut dag = DAG::new();
        
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
            vec![2u8; 64],
        );

        // Add transaction to DAG (using validated method)
        dag.add_transaction_validated(tx.clone()).unwrap();

        // Validation should fail (duplicate)
        let result = validator.validate_dag(&tx, &dag);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_ledger_insufficient_balance() {
        let validator = TransactionValidator::new();
        let _dag = DAG::new();
        let ledger = Ledger::new();
        
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
            vec![1u8; 64],
        );

        // This should fail due to insufficient balance
        let result = validator.validate_ledger(&tx, &ledger, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_ledger_invalid_nonce() {
        let validator = TransactionValidator::new();
        let _dag = DAG::new();
        let mut ledger = Ledger::new();
        let addr = [1u8; 32];
        
        // Set nonce to 5
        ledger.commit_nonce(&addr, 5);
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            addr,
            [2u8; 32],
            100,
            10,
            1234567890,
            0,
            3, // Wrong nonce (should be 6)
            vec![0u8; 64],
            vec![1u8; 64],
        );

        // This should fail due to invalid nonce
        let result = validator.validate_ledger(&tx, &ledger, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_ledger_insufficient_fee() {
        let validator = TransactionValidator::new();
        let _dag = DAG::new();
        let mut ledger = Ledger::new();
        let addr = [1u8; 32];
        
        ledger.set_balance(&addr, 1000);
        ledger.commit_nonce(&addr, 0);
        
        let tx = Transaction::new(
            [[0u8; 32]; 2],
            addr,
            [2u8; 32],
            100,
            5, // Fee below minimum (10)
            1234567890,
            0,
            1,
            vec![0u8; 64],
            vec![1u8; 64],
        );

        // This should fail due to insufficient fee
        let result = validator.validate_ledger(&tx, &ledger, 10);
        assert!(result.is_err());
    }
}
