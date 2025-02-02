pub mod discovery;
pub mod document;
pub mod friends;
pub mod identity;
pub mod phonebook;
pub mod queue;

use std::time::Duration;

use futures::StreamExt;
use rust_ipfs as ipfs;

use ipfs::{Multiaddr, PeerId, Protocol};
use serde::{Deserialize, Serialize};
use warp::{
    crypto::{
        did_key::{CoreSign, Generate},
        DIDKey, Ed25519KeyPair, KeyMaterial, DID,
    },
    error::Error,
    multipass::identity::{Identity, IdentityStatus, Platform},
    tesseract::Tesseract,
};

use crate::config::Discovery;

use self::document::DocumentType;

pub trait VecExt<T: Eq> {
    fn insert_item(&mut self, item: &T) -> bool;
    fn remove_item(&mut self, item: &T) -> bool;
}

impl<T> VecExt<T> for Vec<T>
where
    T: Eq + Clone,
{
    fn insert_item(&mut self, item: &T) -> bool {
        if self.contains(item) {
            return false;
        }

        self.push(item.clone());
        true
    }

    fn remove_item(&mut self, item: &T) -> bool {
        if !self.contains(item) {
            return false;
        }
        if let Some(index) = self.iter().position(|el| item.eq(el)) {
            self.remove(index);
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityPayload {
    pub did: DID,

    /// Type that represents profile picturec
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<DocumentType<String>>,

    /// Type that represents profile banner
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<DocumentType<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<IdentityStatus>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,

    /// Type that represents identity or cid
    pub payload: DocumentType<Identity>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

impl IdentityPayload {
    pub fn sign(mut self, did: &DID) -> Result<IdentityPayload, Error> {
        let bytes = serde_json::to_vec(&self)?;
        let signature = did.sign(&bytes);
        self.signature = Some(signature);
        Ok(self)
    }

    pub fn verify(&self) -> Result<(), Error> {
        let mut payload = self.clone();
        let signature = std::mem::take(&mut payload.signature).ok_or(Error::InvalidSignature)?;

        let bytes = serde_json::to_vec(&payload)?;
        self.did
            .verify(&bytes, &signature)
            .map_err(|_| Error::InvalidSignature)?;
        Ok(())
    }
}

#[allow(deprecated)]
fn did_to_libp2p_pub(public_key: &DID) -> anyhow::Result<ipfs::libp2p::identity::PublicKey> {
    let pub_key =
        ipfs::libp2p::identity::ed25519::PublicKey::decode(&public_key.public_key_bytes())?;
    Ok(ipfs::libp2p::identity::PublicKey::Ed25519(pub_key))
}

fn libp2p_pub_to_did(public_key: &ipfs::libp2p::identity::PublicKey) -> anyhow::Result<DID> {
    let pk = match public_key.clone().into_ed25519() {
        Some(pk) => {
            let did: DIDKey = Ed25519KeyPair::from_public_key(&pk.encode()).into();
            did.try_into()?
        }
        _ => anyhow::bail!(Error::PublicKeyInvalid),
    };
    Ok(pk)
}

fn did_keypair(tesseract: &Tesseract) -> anyhow::Result<DID> {
    let kp = tesseract.retrieve("keypair")?;
    let kp = bs58::decode(kp).into_vec()?;
    let id_kp = warp::crypto::ed25519_dalek::Keypair::from_bytes(&kp)?;
    let did = DIDKey::Ed25519(Ed25519KeyPair::from_secret_key(id_kp.secret.as_bytes()));
    Ok(did.into())
}

// This function stores the topic as a dag in a "discovery:<topic>" format and provide the cid over DHT and obtain the providers of the same cid
// who are providing and connect to them.
// Note that there is usually a delay in `ipfs.provide`.
// TODO: Investigate the delay in providing the CID
pub async fn discovery<S: AsRef<str>>(ipfs: ipfs::Ipfs, topic: S) -> anyhow::Result<()> {
    let topic = topic.as_ref();
    let cid = ipfs
        .put_dag(libipld::ipld!(format!("discovery:{topic}")))
        .await?;
    ipfs.provide(cid).await?;

    loop {
        let mut stream = ipfs.get_providers(cid).await?;
        while let Some(_providers) = stream.next().await {}
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[allow(clippy::large_enum_variant)]
pub enum PeerType {
    PeerId(PeerId),
    DID(DID),
}

impl From<&DID> for PeerType {
    fn from(did: &DID) -> Self {
        PeerType::DID(did.clone())
    }
}

impl From<DID> for PeerType {
    fn from(did: DID) -> Self {
        PeerType::DID(did)
    }
}

impl From<PeerId> for PeerType {
    fn from(peer_id: PeerId) -> Self {
        PeerType::PeerId(peer_id)
    }
}

impl From<&PeerId> for PeerType {
    fn from(peer_id: &PeerId) -> Self {
        PeerType::PeerId(*peer_id)
    }
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
pub enum PeerConnectionType {
    Connected,
    #[default]
    NotConnected,
}

impl From<PeerConnectionType> for IdentityStatus {
    fn from(status: PeerConnectionType) -> Self {
        match status {
            PeerConnectionType::Connected => IdentityStatus::Online,
            PeerConnectionType::NotConnected => IdentityStatus::Offline,
        }
    }
}

pub async fn connected_to_peer<I: Into<PeerType>>(
    ipfs: &ipfs::Ipfs,
    pkey: I,
) -> anyhow::Result<PeerConnectionType> {
    let peer_id = match pkey.into() {
        PeerType::DID(did) => did_to_libp2p_pub(&did)?.to_peer_id(),
        PeerType::PeerId(peer) => peer,
    };

    let connected_peer = ipfs.is_connected(peer_id).await?;

    Ok(match connected_peer {
        true => PeerConnectionType::Connected,
        false => PeerConnectionType::NotConnected,
    })
}

pub async fn discover_peer(
    ipfs: &ipfs::Ipfs,
    did: &DID,
    discovery: Discovery,
    relay: Vec<Multiaddr>,
) -> anyhow::Result<()> {
    let peer_id = did_to_libp2p_pub(did)?.to_peer_id();

    if matches!(
        connected_to_peer(ipfs, PeerType::PeerId(peer_id)).await?,
        PeerConnectionType::Connected,
    ) {
        return Ok(());
    }

    match discovery {
        // Check over DHT to locate peer
        Discovery::Provider(_) | Discovery::Direct => loop {
            if ipfs.identity(Some(peer_id)).await.is_ok() {
                break;
            }
        },
        Discovery::None => {
            //Attempt a direct dial via relay
            for addr in relay.iter() {
                let addr = addr.clone().with(Protocol::P2p(peer_id.into()));
                if let Err(_e) = ipfs.connect(addr).await {
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            loop {
                if connected_to_peer(ipfs, PeerType::PeerId(peer_id)).await?
                    == PeerConnectionType::Connected
                {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    Ok(())
}
