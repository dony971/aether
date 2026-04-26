//! Security Test V1 - Identity Spoofing Protection
//! Tests that the sender/public_key verification blocks identity spoofing attacks

use crate::transaction::Transaction;
use crate::wallet::Wallet;
use ed25519_dalek::{SigningKey, VerifyingKey, Signature};

#[test]
fn test_sender_public_key_match_valid() {
    // Create a valid wallet
    let wallet = Wallet::new();
    let public_key = wallet.public_key_bytes();
    let sender = wallet.address();
    
    // Create a transaction with matching sender and public_key
    let tx = Transaction::new(
        [[0u8; 32]; 2],
        sender,
        [2u8; 32],
        100,
        10,
        1234567890,
        0,
        vec![0u8; 64],
        public_key,
    );
    
    // Should pass verification
    assert!(tx.verify_sender_matches_public_key());
}

#[test]
fn test_sender_public_key_mismatch_invalid() {
    // Create two different wallets
    let wallet1 = Wallet::new();
    let wallet2 = Wallet::new();
    
    let sender1 = wallet1.address();
    let public_key2 = wallet2.public_key_bytes();
    
    // Create a transaction with wallet1's sender but wallet2's public_key
    // This simulates an identity spoofing attack
    let tx = Transaction::new(
        [[0u8; 32]; 2],
        sender1,           // Claiming to be wallet1
        [2u8; 32],
        100,
        10,
        1234567890,
        0,
        vec![0u8; 64],
        public_key2,       // But using wallet2's public_key
    );
    
    // Should fail verification
    assert!(!tx.verify_sender_matches_public_key());
}

#[test]
fn test_sender_public_key_length_check() {
    // Test with invalid public_key length
    let tx = Transaction::new(
        [[0u8; 32]; 2],
        [1u8; 32],
        [2u8; 32],
        100,
        10,
        1234567890,
        0,
        vec![0u8; 64],
        vec![0u8; 16], // Too short
    );
    
    // Should fail verification due to length check
    assert!(!tx.verify_sender_matches_public_key());
}

#[test]
fn test_identity_spoofing_attack_scenario() {
    // Simulate a real attack scenario:
    // Attacker creates transaction claiming to be victim
    
    let victim_wallet = Wallet::new();
    let attacker_wallet = Wallet::new();
    
    let victim_address = victim_wallet.address();
    let attacker_public_key = attacker_wallet.public_key_bytes();
    
    // Attacker signs with their own key
    let signing_key = SigningKey::from_bytes(&attacker_wallet.secret_key_bytes()[..32]).unwrap();
    let dummy_hash = [0u8; 32];
    let signature = signing_key.sign(&dummy_hash);
    
    // Attacker creates transaction claiming to be victim
    let malicious_tx = Transaction::new(
        [[0u8; 32]; 2],
        victim_address,          // Spoofed sender
        [2u8; 32],
        1000000,                  // Large amount
        10,
        1234567890,
        0,
        signature.to_bytes().to_vec(),
        attacker_public_key,      // Attacker's public_key
    );
    
    // V1 fix should block this attack
    assert!(!malicious_tx.verify_sender_matches_public_key());
    
    println!("✅ V1 Fix: Identity spoofing attack successfully blocked");
    println!("   Attacker claimed sender: {}", hex::encode(victim_address));
    println!("   Attacker used public_key: {}", hex::encode(&attacker_public_key[..8]));
    println!("   Verification correctly rejected the transaction");
}
