//! # Networking Module
//!
//! Implements P2P networking using libp2p for transaction propagation and
//! peer discovery.

use libp2p::{
    gossipsub::{self, MessageId, Topic, TopicHash},
    identity::{Keypair, PublicKey},
    mdns,
    multiaddr::Multiaddr,
    swarm::{NetworkBehaviour, Swarm, SwarmBuilder},
    PeerId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use thiserror::Error;

use crate::storage::{Snapshot, StateRoot};

/// DNS seeds for initial peer discovery
/// These are placeholder addresses that will be replaced with production seeds
pub const DNS_SEEDS: &[&str] = &[
    "seed1.aether.network",
    "seed2.aether.network",
    "seed3.aether.network",
    "seed4.aether.network",
];

/// Fixed IP addresses for initial bootstrap (fallback if DNS fails)
/// These are placeholder addresses that will be replaced with production IPs
pub const BOOTSTRAP_IPS: &[&str] = &[
    "/ip4/1.2.3.4/tcp/30333/p2p/12D3KooW...", // Placeholder
    "/ip4/5.6.7.8/tcp/30333/p2p/12D3KooW...", // Placeholder
    "/ip4/9.10.11.12/tcp/30333/p2p/12D3KooW...", // Placeholder
    "/ip4/13.14.15.16/tcp/30333/p2p/12D3KooW...", // Placeholder
];

/// Static seeds for hardcoded bootstrapping
/// These are placeholder addresses for local testing (127.0.0.1:30333)
/// Ready to receive real production IPs when deployed
pub const STATIC_SEEDS: &[&str] = &[
    "/ip4/127.0.0.1/tcp/30333/p2p/12D3KooWLocalTestSeed1",
    "/ip4/127.0.0.1/tcp/30334/p2p/12D3KooWLocalTestSeed2",
    "/ip4/127.0.0.1/tcp/30335/p2p/12D3KooWLocalTestSeed3",
];

/// Bootstrap node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapNode {
    /// Multiaddr of the bootstrap node
    pub multiaddr: String,
    
    /// Peer ID of the bootstrap node (optional, can be discovered)
    pub peer_id: Option<String>,
}

impl BootstrapNode {
    /// Create a new bootstrap node from a multiaddr string
    pub fn from_multiaddr(multiaddr: impl Into<String>) -> Self {
        Self {
            multiaddr: multiaddr.into(),
            peer_id: None,
        }
    }
    
    /// Create a new bootstrap node with peer ID
    pub fn new(multiaddr: impl Into<String>, peer_id: impl Into<String>) -> Self {
        Self {
            multiaddr: multiaddr.into(),
            peer_id: Some(peer_id.into()),
        }
    }
    
    /// Parse the multiaddr
    pub fn parse_multiaddr(&self) -> Result<Multiaddr, NetworkError> {
        self.multiaddr
            .parse::<Multiaddr>()
            .map_err(|e: std::net::AddrParseError| NetworkError::Libp2p(e.to_string()))
    }
}

/// Network error types
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("Libp2p error: {0}")]
    Libp2p(String),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    
    #[error("Channel error: {0}")]
    Channel(String),
    
    #[error("Peer not found")]
    PeerNotFound,
}

/// Network message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// Transaction propagation
    Transaction(Vec<u8>),
    
    /// Block request
    BlockRequest(TransactionId),
    
    /// Block response
    BlockResponse(Option<Vec<u8>>),
    
    /// Peer discovery
    PeerDiscovery(PeerId, Vec<Multiaddr>),
    
    /// Ping
    Ping,
    
    /// Pong
    Pong,
    
    /// Fast Sync: Request latest snapshot
    SnapshotRequest,
    
    /// Fast Sync: Snapshot response
    SnapshotResponse(Option<Snapshot>),
    
    /// Fast Sync: Request state root
    StateRootRequest,
    
    /// Fast Sync: State root response
    StateRootResponse(Option<StateRoot>),
}

/// Transaction ID type alias
pub type TransactionId = [u8; 32];

/// Network behavior combining gossipsub and mDNS
#[derive(NetworkBehaviour)]
struct NetworkBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
}

/// P2P Network node
pub struct P2PNetwork {
    swarm: Swarm<NetworkBehaviour>,
    topic: Topic,
    message_sender: mpsc::UnboundedSender<NetworkMessage>,
    message_receiver: mpsc::UnboundedReceiver<NetworkMessage>,
    peer_addresses: HashMap<PeerId, Vec<Multiaddr>>,
}

impl P2PNetwork {
    /// Create a new P2P network node
    pub async fn new(
        port: u16,
        bootstrap_nodes: Vec<BootstrapNode>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<NetworkMessage>), NetworkError> {
        // Create local key pair
        let local_key = Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        
        // Create transport
        let transport = libp2p::development::p2p::tcp::tokio::Transport::new()
            .upgrade(libp2p::swarm::TransportUpgrade::new(
                libp2p::noise::Config::new(&local_key).unwrap(),
                libp2p::yamux::Config::default(),
            ))
            .boxed();
        
        // Create gossipsub config
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .build()
            .map_err(|e: gossipsub::ConfigBuilderError| NetworkError::Libp2p(e.to_string()))?;
        
        // Create gossipsub behavior
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key.clone()),
            gossipsub_config,
        )
        .map_err(|e: gossipsub::BuildError| NetworkError::Libp2p(e.to_string()))?;
        
        // Create mDNS behavior
        let mdns = mdns::tokio::Behaviour::new(
            mdns::Config::default(),
            local_peer_id,
        )
        .map_err(|e: mdns::Error| NetworkError::Libp2p(e.to_string()))?;
        
        // Create network behavior
        let behaviour = NetworkBehaviour { gossipsub, mdns };
        
        // Create swarm
        let mut swarm = SwarmBuilder::with_tokio_executor(transport, behaviour, local_peer_id)
            .build();
        
        // Listen on the specified port
        let addr = format!("/ip4/0.0.0.0/tcp/{}", port)
            .parse::<Multiaddr>()
            .map_err(|e: std::net::AddrParseError| NetworkError::Libp2p(e.to_string()))?;
        swarm.listen_on(addr).map_err(|e: libp2p::swarm::DialError| NetworkError::Libp2p(e.to_string()))?;
        
        // Create topic
        let topic = Topic::new("dag-network");
        
        // Subscribe to topic
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .map_err(|e: gossipsub::SubscriptionError| NetworkError::Libp2p(e.to_string()))?;
        
        // Create message channels
        let (message_sender, message_receiver) = mpsc::unbounded_channel();
        
        // Dial bootstrap nodes
        for bootstrap_node in bootstrap_nodes {
            let addr = bootstrap_node.parse_multiaddr()?;
            tracing::info!("Dialing bootstrap node: {}", addr);
            swarm.dial(addr.clone()).map_err(|e: libp2p::swarm::DialError| NetworkError::Libp2p(e.to_string()))?;
        }
        
        Ok((
            Self {
                swarm,
                topic,
                message_sender,
                message_receiver,
                peer_addresses: HashMap::new(),
            },
            message_receiver,
        ))
    }
    
    /// Get the local peer ID
    pub fn local_peer_id(&self) -> PeerId {
        *self.swarm.local_peer_id()
    }
    
    /// Get listening addresses
    pub fn listeners(&self) -> Vec<Multiaddr> {
        self.swarm.listeners().cloned().collect()
    }
    
    /// Dial a peer
    pub async fn dial(&mut self, addr: Multiaddr) -> Result<(), NetworkError> {
        self.swarm
            .dial(addr)
            .map_err(|e: libp2p::swarm::DialError| NetworkError::Libp2p(e.to_string()))?;
        Ok(())
    }
    
    /// Broadcast a message to all peers
    pub async fn broadcast(&mut self, message: NetworkMessage) -> Result<(), NetworkError> {
        let serialized = bincode::serialize(&message)?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), serialized)
            .map_err(|e: gossipsub::PublishError| NetworkError::Libp2p(e.to_string()))?;
        Ok(())
    }
    
    /// Send a message to a specific peer
    pub async fn send_to_peer(
        &mut self,
        peer_id: PeerId,
        message: NetworkMessage,
    ) -> Result<(), NetworkError> {
        let serialized = bincode::serialize(&message)?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), serialized)
            .map_err(|e: gossipsub::PublishError| NetworkError::Libp2p(e.to_string()))?;
        Ok(())
    }
    
    /// Add a peer address
    pub fn add_peer_address(&mut self, peer_id: PeerId, addr: Multiaddr) {
        self.peer_addresses
            .entry(peer_id)
            .or_insert_with(Vec::new)
            .push(addr);
    }
    
    /// Get peer addresses
    pub fn get_peer_addresses(&self, peer_id: PeerId) -> Option<&Vec<Multiaddr>> {
        self.peer_addresses.get(&peer_id)
    }
    
    /// Get connected peers
    pub fn connected_peers(&self) -> Vec<PeerId> {
        self.swarm
            .behaviour()
            .gossipsub
            .peers()
            .cloned()
            .collect()
    }
    
    /// Create a new P2P network node without bootstrap nodes
    pub async fn new_without_bootstrap(
        port: u16,
    ) -> Result<(Self, mpsc::UnboundedReceiver<NetworkMessage>), NetworkError> {
        Self::new(port, Vec::new()).await
    }
    
    /// Request latest snapshot from peers (Fast Sync)
    pub async fn request_snapshot(&mut self) -> Result<(), NetworkError> {
        let message = NetworkMessage::SnapshotRequest;
        self.broadcast(message).await?;
        Ok(())
    }
    
    /// Request state root from peers (Fast Sync)
    pub async fn request_state_root(&mut self) -> Result<(), NetworkError> {
        let message = NetworkMessage::StateRootRequest;
        self.broadcast(message).await?;
        Ok(())
    }
    
    /// Send snapshot to peer
    pub async fn send_snapshot(&mut self, snapshot: Snapshot) -> Result<(), NetworkError> {
        let message = NetworkMessage::SnapshotResponse(Some(snapshot));
        self.broadcast(message).await?;
        Ok(())
    }
    
    /// Send state root to peer
    pub async fn send_state_root(&mut self, state_root: StateRoot) -> Result<(), NetworkError> {
        let message = NetworkMessage::StateRootResponse(Some(state_root));
        self.broadcast(message).await?;
        Ok(())
    }
    
    /// Run the network event loop
    pub async fn run(&mut self) -> Result<(), NetworkError> {
        loop {
            match self.swarm.select_next_some().await {
                libp2p::swarm::SwarmEvent::Behaviour(event) => {
                    match event {
                        NetworkBehaviourEvent::Gossipsub(gossipsub_event) => {
                            match gossipsub_event {
                                gossipsub::Event::Message {
                                    propagation_source,
                                    message_id: _,
                                    message,
                                } => {
                                    // Handle incoming message
                                    if let Ok(network_message) = bincode::deserialize::<NetworkMessage>(&message.data) {
                                        let _ = self.message_sender.send(network_message);
                                    }
                                }
                                gossipsub::Event::Subscribed { peer_id, topic } => {
                                    tracing::info!("Peer {} subscribed to {:?}", peer_id, topic);
                                }
                                gossipsub::Event::Unsubscribed { peer_id, topic } => {
                                    tracing::info!("Peer {} unsubscribed from {:?}", peer_id, topic);
                                }
                                _ => {}
                            }
                        }
                        NetworkBehaviourEvent::Mdns(mdns_event) => {
                            match mdns_event {
                                mdns::Event::Discovered(list) => {
                                    for (peer_id, addr) in list {
                                        self.add_peer_address(peer_id, addr.clone());
                                        let _ = self.swarm.dial(addr);
                                    }
                                }
                                mdns::Event::Expired(list) => {
                                    for (peer_id, _) in list {
                                        self.peer_addresses.remove(&peer_id);
                                    }
                                }
                            }
                        }
                    }
                }
                libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } => {
                    tracing::info!("Listening on {}", address);
                }
                libp2p::swarm::SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    tracing::info!("Connected to peer: {}", peer_id);
                }
                libp2p::swarm::SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    tracing::info!("Disconnected from peer: {}", peer_id);
                }
                _ => {}
            }
        }
    }
}

/// Network event from the message channel
#[derive(Debug)]
pub enum NetworkEvent {
    /// Received a message
    Message(NetworkMessage),
    
    /// Peer connected
    PeerConnected(PeerId),
    
    /// Peer disconnected
    PeerDisconnected(PeerId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_message_serialization() {
        let message = NetworkMessage::Ping;
        let serialized = bincode::serialize(&message).unwrap();
        let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
        
        assert!(matches!(deserialized, NetworkMessage::Ping));
    }

    #[test]
    fn test_transaction_message() {
        let tx_data = vec![1u8, 2, 3, 4];
        let message = NetworkMessage::Transaction(tx_data.clone());
        
        let serialized = bincode::serialize(&message).unwrap();
        let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
        
        match deserialized {
            NetworkMessage::Transaction(data) => {
                assert_eq!(data, tx_data);
            }
            _ => panic!("Wrong message type"),
        }
    }
    
    #[test]
    fn test_bootstrap_node_creation() {
        let bootstrap = BootstrapNode::from_multiaddr("/ip4/127.0.0.1/tcp/30333");
        assert_eq!(bootstrap.multiaddr, "/ip4/127.0.0.1/tcp/30333");
        assert!(bootstrap.peer_id.is_none());
    }
    
    #[test]
    fn test_bootstrap_node_with_peer_id() {
        let bootstrap = BootstrapNode::new(
            "/ip4/127.0.0.1/tcp/30333",
            "12D3KooW..."
        );
        assert_eq!(bootstrap.multiaddr, "/ip4/127.0.0.1/tcp/30333");
        assert_eq!(bootstrap.peer_id, Some("12D3KooW...".to_string()));
    }
}
