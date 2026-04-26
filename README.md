# AETHER SEDC - Self-Evolving DAG Consensus

**Version**: 1.0.0  
**Statut**: Unifié et Prêt pour Compilation

---

## 🚀 AETHER SEDC - Protocole Blockchain Révolutionnaire

AETHER SEDC est un protocole de consensus DAG révolutionnaire qui élimine les blocs séquentiels et le consensus basé sur des leaders.

### 🌟 Innovations Uniques

1. **Architecture Blockless** - Pas de blocs, seulement des transactions comme nœuds DAG
2. **Consensus Sans Leader** - Heavy Subgraph Consensus (pas de mining, pas de validators élus)
3. **Finalité Adaptative** - Finalité probabilistique qui s'adapte aux conditions réseau
4. **Économie Liée au Consensus** - Récompenses SEULEMENT pour transactions finalisées
5. **Réputation Dynamique** - Système de réputation qui évolue selon le comportement

### 📊 Comparaison avec Bitcoin

| Caractéristique | Bitcoin | AETHER SEDC |
|---|---|---|
| Architecture | Blocs séquentiels | DAG blockless |
| Consensus | Proof-of-Work | Heavy Subgraph Consensus |
| Leaders | Miners | Aucun (leaderless) |
| Finalité | 6 confirmations fixes | Adaptative probabilistique |
| Économie | Indépendante de finalité | Strictement liée au consensus |
| Débit | ~7 TPS | Illimité (théoriquement) |

---

## 🏗️ Architecture du Projet Unifié

Le projet `aether-unified` fusionne:
- **Infrastructure complète** de `dag-network` (storage, RPC, P2P, wallet, etc.)
- **Algorithmes de consensus avancés** de `sedc-core` (Heavy Subgraph Scoring, Reputation System)

### Modules Principaux

```
src/
├── transaction.rs          # Structure de transaction et signature
├── parent_selection.rs     # DAG storage et tip selection
├── consensus.rs            # Consensus State + Heavy Subgraph Scoring
├── reputation.rs          # Système de réputation dynamique (NOUVEAU)
├── ledger.rs              # Gestion des soldes et politique monétaire
├── validation.rs          # Pipeline de validation (3 étapes)
├── transaction_processor.rs # Zero-trust single entry point
├── storage.rs             # Persistance Sled DB
├── p2p.rs                 # Réseau P2P gossip-based
├── rpc.rs                 # API JSON-RPC
├── wallet.rs              # Gestion de wallet
├── economics.rs           # Module économique
├── pow.rs                 # Micro-PoW anti-spam
├── genesis.rs             # Initialisation genesis
└── main.rs                # Point d'entrée du noeud
```

---

## 🔒 Sécurité Renforcée

### Pipeline de Validation Strict
```
1. validate_pure(tx)      - Validation structurelle (pas d'accès état)
2. validate_dag(tx, dag)  - Validation parents, double-spend (lecture seule)
3. validate_ledger(tx)    - Validation solde, nonce, fee (lecture seule)
4. Exécution atomique     - Write locks + rollback sur erreur
```

### Invariants de Sécurité
- ❌ Pas de mutation avant validation complète
- ❌ Pas de bypass des validation layers
- ❌ Pas de block_height manuel dans RPC
- ✅ Single source of truth: ConsensusState
- ✅ Atomicité avec rollback
- ✅ Fork-safe rewards via BlockId tracking

### Système de Réputation
- Réputation initiale: 0.5
- +0.01 pour transaction validée correctement
- -0.5 pour double-spend confirmé
- -0.1 pour transaction invalide
- Décroissance temporelle: 0.001 par heure
- Seuil minimum: 0.1 pour validation
- Discount sur frais si réputation ≥ 0.7

---

## 💰 Politique Monétaire

- **MAX_SUPPLY**: 21,000,000 AETH
- **Unités**: 10 décimales (1 AETH = 10^10 unités)
- **Récompense initiale**: 10 AETH
- **Halving**: Tous les 210,000 blocs
- **FEE_BURN_ADDRESS**: [0xFFu8; 32]

### Récompenses Liées au Consensus
```
reward_issued(tx) = true iff state(tx) = Finalized
```

---

## 🛠️ Compilation

### Prérequis
- Rust 1.70 ou supérieur
- Windows 10/11, Linux, ou macOS

### Instructions

```bash
# Naviguer dans le projet
cd aether-unified

# Compiler le projet
cargo build --release

# Exécuter les tests
cargo test --lib

# Lancer le noeud
cargo run --release
```

### Résoudre les problèmes de compilation

Si vous rencontrez des erreurs de fichiers verrouillés:
```bash
# Fermer tous les processus Rust
taskkill /F /IM rustc.exe /T
taskkill /F /IM cargo.exe /T

# Nettoyer le dossier target
Remove-Item -Recurse -Force target

# Recompiler
cargo build --release
```

---

## 🧪 Tests

Le projet inclut 159 tests de sécurité couvrant:
- ✅ Replay attack prevention
- ✅ Double spend prevention
- ✅ Fork safety
- ✅ Atomic execution rollback
- ✅ Orphan recovery
- ✅ Monetary policy enforcement
- ✅ Consensus state invariants

```bash
# Exécuter tous les tests
cargo test --lib

# Exécuter uniquement les tests de sécurité
cargo test --lib security_tests
```

---

## 📝 API RPC

Le noeud expose une API JSON-RPC sur le port 8545 (par défaut).

### Méthodes Principales

```json
// Soumettre une transaction
{
  "jsonrpc": "2.0",
  "method": "aether_submitTransaction",
  "params": [{ "transaction": "..." }],
  "id": 1
}

// Obtenir le solde
{
  "jsonrpc": "2.0",
  "method": "aether_getBalance",
  "params": ["0x..."],
  "id": 1
}

// Obtenir les tips du DAG
{
  "jsonrpc": "2.0",
  "method": "aether_getTips",
  "params": [],
  "id": 1
}
```

---

## 🔬 Fonctionnalités Avancées

### Heavy Subgraph Scoring
```rust
score(S) = Σ (weight(tx) × reputation(validator) × depth_factor)
depth_factor = 1.0 + (depth(tx) / max_depth) × 0.5
```

### Finalité Adaptative
```rust
P_finality(tx) = 1 - exp(-λ × confirmations / volatility)
```

### Frais Dynamiques
```rust
fee = max(base_fee, adaptive_fee)
adaptive_fee = base_fee × (1 + density_multiplier) × (1 - reputation_discount)
```

---

## 📈 Feuille de Route

### ✅ Complété
- [x] Infrastructure DAG complète
- [x] Système de validation strict
- [x] Transaction processor atomique
- [x] Système de réputation dynamique
- [x] Heavy Subgraph Scoring
- [x] Finalité adaptative
- [x] Orphan recovery persistant
- [x] Tests de sécurité (159 tests)

### 🚧 En Cours
- [ ] P2P conflict resolution
- [ ] Integration Micro-PoW avec validation
- [ ] Module économique complet (frais dynamiques)

### 📋 À Faire
- [ ] Explorer API
- [ ] Wallet UI
- [ ] Monitoring (Prometheus)
- [ ] Documentation complète
- [ ] Audit externe
- [ ] Déploiement testnet

---

## 🤝 Contribution

Pour contribuer:
1. Fork le projet
2. Créer une branche pour votre fonctionnalité
3. Commit vos changements
4. Push vers la branche
5. Ouvrir une Pull Request

---

## 📄 Licence

MIT License - Voir le fichier LICENSE pour les détails

---

## 📞 Contact

- **Projet**: AETHER SEDC
- **Version**: 1.0.0
- **Date**: 26 Avril 2026

---

## 🎯 Objectif

Créer un système blockchain véritablement décentralisé, sans leaders, avec une finalité adaptative et une économie strictement liée au consensus pour une sécurité zero-trust.

**"Le futur de la blockchain n'a pas de blocs."**
