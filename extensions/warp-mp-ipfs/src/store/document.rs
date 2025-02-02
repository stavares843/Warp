use futures::StreamExt;
use ipfs::{Ipfs, IpfsPath};
use libipld::{
    serde::{from_ipld, to_ipld},
    Cid,
};
use rust_ipfs as ipfs;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{hash::Hash, time::Duration};
use warp::{
    crypto::{did_key::CoreSign, DID},
    error::Error,
    multipass::identity::{Identity, IdentityStatus, Platform},
};

use super::friends::Request;

#[async_trait::async_trait]
pub(crate) trait ToCid: Sized {
    async fn to_cid(&self, ipfs: &Ipfs) -> Result<Cid, Error>;
}

#[async_trait::async_trait]
pub(crate) trait GetDag<D>: Sized {
    async fn get_dag(&self, ipfs: &Ipfs, timeout: Option<Duration>) -> Result<D, Error>;
}

#[async_trait::async_trait]
impl<D: DeserializeOwned> GetDag<D> for Cid {
    async fn get_dag(&self, ipfs: &Ipfs, timeout: Option<Duration>) -> Result<D, Error> {
        IpfsPath::from(*self).get_dag(ipfs, timeout).await
    }
}

#[async_trait::async_trait]
impl<D: DeserializeOwned> GetDag<D> for IpfsPath {
    async fn get_dag(&self, ipfs: &Ipfs, timeout: Option<Duration>) -> Result<D, Error> {
        let timeout = timeout.unwrap_or(std::time::Duration::from_secs(10));
        match tokio::time::timeout(timeout, ipfs.get_dag(self.clone())).await {
            Ok(Ok(ipld)) => from_ipld(ipld)
                .map_err(anyhow::Error::from)
                .map_err(Error::from),
            Ok(Err(e)) => Err(Error::Any(e)),
            Err(e) => Err(Error::from(anyhow::anyhow!("Timeout at {e}"))),
        }
    }
}

#[async_trait::async_trait]
impl<T> ToCid for T
where
    T: Serialize + Clone + Send + Sync,
{
    async fn to_cid(&self, ipfs: &Ipfs) -> Result<Cid, Error> {
        let ipld = to_ipld(self.clone()).map_err(anyhow::Error::from)?;
        ipfs.put_dag(ipld).await.map_err(Error::from)
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DocumentType<T> {
    Object(T),
    Cid(Cid),
    UnixFS(Cid, Option<usize>),
}

impl<T> DocumentType<T> {
    pub async fn resolve(&self, ipfs: Ipfs, timeout: Option<Duration>) -> Result<T, Error>
    where
        T: Clone,
        T: DeserializeOwned,
    {
        match self {
            DocumentType::Object(object) => Ok(object.clone()),
            DocumentType::Cid(cid) => {
                let timeout = timeout.unwrap_or(std::time::Duration::from_secs(30));
                match tokio::time::timeout(timeout, ipfs.get_dag(IpfsPath::from(*cid))).await {
                    Ok(Ok(ipld)) => from_ipld::<T>(ipld)
                        .map_err(anyhow::Error::from)
                        .map_err(Error::from),
                    Ok(Err(e)) => Err(Error::Any(e)),
                    Err(e) => Err(Error::from(anyhow::anyhow!("Timeout at {e}"))),
                }
            }
            //This will resolve into a buffer that can be deserialize into T.
            //Best not to use this to resolve a large file.
            DocumentType::UnixFS(cid, limit) => {
                let fut = async {
                    let stream = ipfs
                        .cat_unixfs(IpfsPath::from(*cid), None)
                        .await
                        .map_err(anyhow::Error::from)?;

                    futures::pin_mut!(stream);

                    let mut data = vec![];

                    while let Some(stream) = stream.next().await {
                        if let Some(limit) = limit {
                            if data.len() >= *limit {
                                return Err(Error::InvalidLength {
                                    context: "data".into(),
                                    current: data.len(),
                                    minimum: None,
                                    maximum: Some(*limit),
                                });
                            }
                        }
                        match stream {
                            Ok(bytes) => {
                                data.extend(bytes);
                            }
                            Err(e) => return Err(Error::from(anyhow::anyhow!("{e}"))),
                        }
                    }
                    Ok(data)
                };

                let timeout = timeout.unwrap_or(std::time::Duration::from_secs(15));
                match tokio::time::timeout(timeout, fut).await {
                    Ok(Ok(data)) => serde_json::from_slice(&data).map_err(Error::from),
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(Error::from(anyhow::anyhow!("Timeout at {e}"))),
                }
            }
        }
    }

    pub async fn resolve_or_default(&self, ipfs: Ipfs, timeout: Option<Duration>) -> T
    where
        T: Clone,
        T: DeserializeOwned,
        T: Default,
    {
        self.resolve(ipfs, timeout).await.unwrap_or_default()
    }
}

impl<T> From<Cid> for DocumentType<T> {
    fn from(cid: Cid) -> Self {
        DocumentType::Cid(cid)
    }
}

/// node root document for their identity, friends, blocks, etc, along with previous cid (if we wish to track that)
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct RootDocument {
    //TODO: Maybe use DocumentType<Identity>?
    pub identity: Cid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<DocumentType<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<DocumentType<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friends: Option<DocumentType<Vec<DID>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<DocumentType<Vec<DID>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_by: Option<DocumentType<Vec<DID>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<DocumentType<Vec<Request>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<IdentityStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl RootDocument {
    pub fn sign(&mut self, did: &DID) -> Result<(), Error> {
        let mut root_document = self.clone();
        //In case there is a signature already exist
        root_document.signature = None;
        let bytes = serde_json::to_vec(&root_document)?;
        let signature = did.sign(&bytes);
        self.signature = Some(bs58::encode(signature).into_string());
        Ok(())
    }

    pub async fn verify(&self, ipfs: &Ipfs) -> Result<(), Error> {
        let (identity, _, _, _, _, _, _) = self.resolve(ipfs, Some(Duration::from_secs(5))).await?;
        let mut root_document = self.clone();
        let signature =
            std::mem::take(&mut root_document.signature).ok_or(Error::InvalidSignature)?;
        let bytes = serde_json::to_vec(&root_document)?;
        let sig = bs58::decode(&signature).into_vec()?;
        identity
            .did_key()
            .verify(&bytes, &sig)
            .map_err(|_| Error::InvalidSignature)?;
        Ok(())
    }

    pub async fn resolve(
        &self,
        ipfs: &Ipfs,
        timeout: Option<Duration>,
    ) -> Result<
        (
            Identity,
            String,
            String,
            Vec<DID>,
            Vec<DID>,
            Vec<DID>,
            Vec<Request>,
        ),
        Error,
    > {
        let identity = {
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                ipfs.get_dag(IpfsPath::from(self.identity)),
            )
            .await
            {
                Ok(Ok(ipld)) => from_ipld::<Identity>(ipld)
                    .map_err(anyhow::Error::from)
                    .map_err(Error::from)?,
                Ok(Err(_)) => return Err(Error::IdentityInvalid),
                Err(e) => return Err(Error::from(anyhow::anyhow!("Timeout at {e}"))),
            }
        };

        let mut friends = Default::default();
        let mut picture = Default::default();
        let mut banner = Default::default();
        let mut block_list = Default::default();
        let mut block_by_list = Default::default();
        let mut request = Default::default();

        if let Some(document) = &self.friends {
            friends = document.resolve_or_default(ipfs.clone(), timeout).await
        }

        if let Some(document) = &self.picture {
            picture = document.resolve_or_default(ipfs.clone(), timeout).await
        }

        if let Some(document) = &self.banner {
            banner = document.resolve_or_default(ipfs.clone(), timeout).await
        }

        if let Some(document) = &self.blocks {
            block_list = document.resolve_or_default(ipfs.clone(), timeout).await
        }

        if let Some(document) = &self.block_by {
            block_by_list = document.resolve_or_default(ipfs.clone(), timeout).await
        }

        if let Some(document) = &self.request {
            request = document.resolve_or_default(ipfs.clone(), timeout).await
        }

        Ok((
            identity,
            picture,
            banner,
            friends,
            block_list,
            block_by_list,
            request,
        ))
    }
}

/// Used to lookup identities found and their corresponding cid
#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
pub struct CacheDocument {
    pub username: String,
    pub did: DID,
    pub short_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<DocumentType<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<DocumentType<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<IdentityStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    pub identity: DocumentType<Identity>,
}

impl CacheDocument {
    pub async fn resolve(&self, ipfs: Ipfs, timeout: Option<Duration>) -> Result<Identity, Error> {
        let mut identity = self.identity.resolve(ipfs.clone(), timeout).await?;
        if identity.username() != self.username.clone()
            || identity.did_key() != self.did.clone()
            || identity.short_id() != self.short_id
        {
            return Err(Error::IdentityInvalid);
        }

        if let Some(document) = &self.picture {
            let mut lazily = false;
            if let DocumentType::UnixFS(cid, _) = document {
                if !ipfs.refs_local().await?.contains(cid) {
                    tokio::spawn({
                        let ipfs = ipfs.clone();
                        let document = document.clone();
                        async move {
                            document.resolve_or_default(ipfs, timeout).await;
                        }
                    });
                    lazily = true;
                }
            }
            if !lazily {
                let picture = document.resolve_or_default(ipfs.clone(), timeout).await;
                let mut graphics = identity.graphics();
                graphics.set_profile_picture(&picture);
                identity.set_graphics(graphics);
            }
        }
        if let Some(document) = &self.banner {
            let mut lazily = false;
            if let DocumentType::UnixFS(cid, _) = document {
                if !ipfs.refs_local().await?.contains(cid) {
                    tokio::spawn({
                        let ipfs = ipfs.clone();
                        let document = document.clone();
                        async move {
                            document.resolve_or_default(ipfs, timeout).await;
                        }
                    });
                    lazily = true;
                }
            }
            if !lazily {
                let banner = document.resolve_or_default(ipfs.clone(), timeout).await;
                let mut graphics = identity.graphics();
                graphics.set_profile_banner(&banner);
                identity.set_graphics(graphics);
            }
        }

        Ok(identity)
    }
}

impl Hash for CacheDocument {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.did.hash(state);
        self.short_id.hash(state);
    }
}

impl PartialEq for CacheDocument {
    fn eq(&self, other: &Self) -> bool {
        self.did.eq(&other.did) && self.short_id.eq(&other.short_id)
    }
}
