// Used to ignore unused variables, mostly related to ones in the trait functions
//TODO: Remove
//TODO: Use rust-ipfs branch with major changes for pubsub, ipld, etc
#![allow(unused_variables)]
#![allow(unused_imports)]

pub mod store;
pub mod config;

use anyhow::bail;
use config::Config;
use futures::{Future, TryFutureExt};
use ipfs::ipld::ipld_macro;
use serde::de::DeserializeOwned;
use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use warp::data::{DataObject, DataType};
use warp::pocket_dimension::query::QueryBuilder;
use warp::sync::{Arc, Mutex, MutexGuard};

use warp::module::Module;
use warp::pocket_dimension::PocketDimension;
use warp::tesseract::Tesseract;
use warp::{Extension, SingleHandle};

use ipfs::ipld::dag_json::DagJsonCodec;
use ipfs::{
    make_ipld, Block, Cid, Ipfs, IpfsOptions, IpfsPath, Ipld, Keypair, Types, UninitializedIpfs, Protocol, PeerId,
};
use libp2p::multihash::Sha2_256;
use tokio::sync::mpsc::Sender;
use warp::crypto::rand::Rng;
use warp::crypto::PublicKey;
use warp::error::Error;
use warp::multipass::generator::generate_name;
use warp::multipass::identity::{FriendRequest, Identifier, Identity, IdentityUpdate};
use warp::multipass::{identity, Friends, MultiPass};

pub struct IpfsIdentity {
    path: PathBuf,
    cache: Option<Arc<Mutex<Box<dyn PocketDimension>>>>,
    tesseract: Tesseract,
    ipfs: Ipfs<Types>,
    temp: bool,
    keypair: Keypair,
    //TODO: FriendStore
    //      * Add/Remove/Block friends
    //      * Show incoming/outgoing request
    //TODO: AccountManager
    //      * Account registry (for self)
    //      * Account lookup
    //      * Profile information
}

impl Drop for IpfsIdentity {
    fn drop(&mut self) {
        // We want to gracefully close the ipfs repo to allow for any cleanup
        async_block_unchecked(self.ipfs.clone().exit_daemon());

        // If IpfsIdentity::temporary was used, `temp` would be true and it would
        // let is to delete the repo
        if self.temp {
            if let Ok(_) = std::fs::remove_dir_all(&self.path) {}
        }
    }
}

impl IpfsIdentity {
    pub async fn temporary(
        config: Option<Config>,
        tesseract: Tesseract,
        cache: Option<Arc<Mutex<Box<dyn PocketDimension>>>>,
    ) -> anyhow::Result<IpfsIdentity> {
        if let Some(config) = &config {
            if config.path.is_some() { anyhow::bail!("Path cannot be set") }
        }
        IpfsIdentity::new( config.unwrap_or_default(), tesseract, cache).await
    }

    pub async fn persistent<P: AsRef<std::path::Path>>(
        config: Config,
        tesseract: Tesseract,
        cache: Option<Arc<Mutex<Box<dyn PocketDimension>>>>,
    ) -> anyhow::Result<IpfsIdentity> {
        if config.path.is_none() { anyhow::bail!("Path is required for identity to be persistent") }
        IpfsIdentity::new(config, tesseract, cache).await
    }

    pub async fn new(
        config: Config,
        tesseract: Tesseract,
        cache: Option<Arc<Mutex<Box<dyn PocketDimension>>>>,
    ) -> anyhow::Result<IpfsIdentity> {

        let keypair = match tesseract.retrieve("ipfs_keypair") {
            Ok(keypair) => {
                let kp = bs58::decode(keypair).into_vec()?;
                let id_kp = warp::crypto::ed25519_dalek::Keypair::from_bytes(&kp)?;
                let secret =
                    libp2p::identity::ed25519::SecretKey::from_bytes(id_kp.secret.to_bytes())?;
                Keypair::Ed25519(secret.into())
            }
            Err(_) => Keypair::generate_ed25519(),
        };

        let temp = config.path.is_none();
        let path = config.path.unwrap_or_else(|| {
            let temp = warp::crypto::rand::thread_rng().gen_range(0, 1000);
            std::env::temp_dir().join(&format!("ipfs-temp-{temp}"))
        });

        let mut bootstrap = vec![];
        
        for addr in config.bootstrap {
            let mut addr = addr.clone();
            let peer_id = match addr.pop() {
                Some(Protocol::P2p(hash)) => match PeerId::from_multihash(hash) {
                    Ok(id) => id,
                    Err(_) => {
                        continue;
                    }
                },
                _ => {
                    continue; 
                }
            };
            bootstrap.push((addr, peer_id));
        }

        let opts = IpfsOptions {
            ipfs_path: path.clone(),
            keypair: keypair.clone(),
            bootstrap,
            mdns: false,
            kad_protocol: None,
            listening_addrs: config.listen_on,
            span: None,
        };

        // Create directory if it doesnt exist
        if !opts.ipfs_path.exists() {
            tokio::fs::create_dir(opts.ipfs_path.clone()).await?;
        }

        let (ipfs, fut) = UninitializedIpfs::new(opts).start().await?;
        tokio::task::spawn(fut);

        //TODO: Manually load bootstrap or use IpfsOptions
        ipfs.restore_bootstrappers().await?;

        Ok(IpfsIdentity {
            path,
            tesseract,
            cache,
            ipfs,
            keypair,
            temp,
        })
    }

    pub fn get_cache(&self) -> anyhow::Result<MutexGuard<Box<dyn PocketDimension>>> {
        let cache = self
            .cache
            .as_ref()
            .ok_or(Error::PocketDimensionExtensionUnavailable)?;

        Ok(cache.lock())
    }

    pub fn raw_keypair(&self) -> anyhow::Result<libp2p::identity::ed25519::Keypair> {
        match self.keypair.clone() {
            Keypair::Ed25519(kp) => Ok(kp),
            _ => bail!("Unsupported keypair"),
        }
    }
}

pub fn async_block<F: Future>(fut: F) -> anyhow::Result<F::Output> {
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle,
        Err(_) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .handle()
            .clone(),
    };
    Ok(tokio::task::block_in_place(|| handle.block_on(fut)))
}

pub fn async_block_unchecked<F: Future>(fut: F) -> F::Output {
    async_block(fut).expect("Unable to run future on runtime")
}

impl Extension for IpfsIdentity {
    fn id(&self) -> String {
        "warp-mp-ipfs".to_string()
    }
    fn name(&self) -> String {
        "Ipfs Identity".into()
    }

    fn module(&self) -> Module {
        Module::Accounts
    }
}

impl SingleHandle for IpfsIdentity {
    fn handle(&self) -> Result<Box<dyn Any>, Error> {
        Ok(Box::new(self.ipfs.clone()))
    }
}

impl MultiPass for IpfsIdentity {
    fn create_identity(
        &mut self,
        username: Option<&str>,
        passphrase: Option<&str>,
    ) -> Result<PublicKey, Error> {
        if let Ok(encoded_kp) = self.tesseract.retrieve("ipfs_keypair") {
            let kp = bs58::decode(encoded_kp)
                .into_vec()
                .map_err(anyhow::Error::from)?;
            let keypair = warp::crypto::ed25519_dalek::Keypair::from_bytes(&kp)?;

            //TODO: Check records to determine if profile exist properly
            if let Ok(cid) = self.tesseract.retrieve("root_cid") {
                let cid: Cid = cid.parse().map_err(anyhow::Error::from)?;
                let path = IpfsPath::from(cid);
                let identity_path = path.sub_path("identity").map_err(anyhow::Error::from)?;
                //TODO: Fix deadlock if cid doesnt exist. May be related to the ipld link
                let identity = match async_block_unchecked(self.ipfs.get_dag(identity_path)) {
                    Ok(Ipld::Bytes(bytes)) => serde_json::from_slice::<Identity>(&bytes)?,
                    _ => return Err(Error::Other), //Note: It should not hit here unless the repo is corrupted
                };
                let public_key = identity.public_key();
                let inner_pk = PublicKey::from_bytes(keypair.public.as_ref());

                if public_key == inner_pk {
                    return Err(Error::IdentityExist);
                }
            }
        }

        let raw_kp = self.raw_keypair()?;

        let mut identity = Identity::default();
        let public_key = PublicKey::from_bytes(&raw_kp.public().encode());

        let username = match username {
            Some(u) => u.to_string(),
            None => generate_name(),
        };

        identity.set_username(&username);
        identity.set_short_id(warp::crypto::rand::thread_rng().gen_range(0, 9999));
        identity.set_public_key(public_key);
        // Convert our identity to ipld. This step would convert it to serde_json::Value then match accordingly
        // Update `to_ipld`
        // let ipld_val = to_ipld(identity.clone())?;
        let bytes = serde_json::to_vec(&identity)?;
        // Store the identity as a dag
        let ident_cid = async_block_unchecked(self.ipfs.put_dag(make_ipld!(bytes)))?;
        let root_handle =
            async_block_unchecked(self.ipfs.put_dag(make_ipld!({ "identity": ident_cid })))?;

        // Pin the dag
        async_block_unchecked(self.ipfs.insert_pin(&root_handle, false))?;

        // Note that for the time being we will be storing the Cid to tesseract,
        // however this would need to be handled a different way, especially since the cid is stored in the pinstore
        // in rust-ipfs.
        // TODO: Store the Cid of the root handle properly
        // TODO: Provide the Cid to DHT. Either through the PutProvider or (soon to be implemented) ipns
        self.tesseract.set("root_cid", &root_handle.to_string())?;
        let encoded_kp = bs58::encode(&raw_kp.encode()).into_string();

        self.tesseract.set("ipfs_keypair", &encoded_kp)?;
        if let Ok(mut cache) = self.get_cache() {
            let object = DataObject::new(DataType::from(Module::Accounts), &identity)?;
            cache.add_data(DataType::from(Module::Accounts), &object)?;
        }
        Ok(identity.public_key())
    }

    //TODO: Use DHT to perform lookups
    fn get_identity(&self, id: Identifier) -> Result<Identity, Error> {
        match id.get_inner() {
            (Some(pk), None, false) => {
                if let Ok(cache) = self.get_cache() {
                    let mut query = QueryBuilder::default();
                    query.r#where("public_key", &pk)?;
                    if let Ok(list) = cache.get_data(DataType::from(Module::Accounts), Some(&query))
                    {
                        //get last
                        if !list.is_empty() {
                            let obj = list.last().unwrap();
                            return obj.payload::<Identity>();
                        }
                    }
                }
                return Err(Error::IdentityDoesntExist);
            }
            (None, Some(username), false) => {
                if let Ok(cache) = self.get_cache() {
                    let mut query = QueryBuilder::default();
                    query.r#where("username", &username)?;
                    if let Ok(list) = cache.get_data(DataType::from(Module::Accounts), Some(&query))
                    {
                        //get last
                        if !list.is_empty() {
                            let obj = list.last().unwrap();
                            return obj.payload::<Identity>();
                        }
                    }
                }
                //TODO: Lookup by username
                return Err(Error::IdentityDoesntExist);
            }
            (None, None, true) => {
                match self.tesseract.retrieve("root_cid") {
                    Ok(cid) => {
                        let cid: Cid = cid.parse().map_err(anyhow::Error::from)?;
                        let path = IpfsPath::from(cid);
                        let identity_path =
                            path.sub_path("identity").map_err(anyhow::Error::from)?;
                        let identity = match async_block_unchecked(self.ipfs.get_dag(identity_path))
                        {
                            Ok(Ipld::Bytes(bytes)) => serde_json::from_slice::<Identity>(&bytes)?,
                            _ => return Err(Error::Other), //Note: It should not hit here unless the repo is corrupted
                        };
                        Ok(identity)
                    }
                    Err(_) => Err(Error::IdentityDoesntExist),
                }
            }
            _ => return Err(Error::InvalidIdentifierCondition),
        }
    }

    fn update_identity(&mut self, option: IdentityUpdate) -> Result<(), Error> {
        let mut identity = self.get_own_identity()?;
        let old_identity = identity.clone();
        match (
            option.username(),
            option.graphics_picture(),
            option.graphics_banner(),
            option.status_message(),
        ) {
            (Some(username), None, None, None) => identity.set_username(&username),
            (None, Some(hash), None, None) => {
                let mut graphics = identity.graphics();
                graphics.set_profile_picture(&hash);
                identity.set_graphics(graphics);
            }
            (None, None, Some(hash), None) => {
                let mut graphics = identity.graphics();
                graphics.set_profile_banner(&hash);
                identity.set_graphics(graphics);
            }
            (None, None, None, Some(status)) => identity.set_status_message(status),
            _ => return Err(Error::CannotUpdateIdentity),
        }

        match self.tesseract.retrieve("root_cid") {
            Ok(cid) => {
                let cid: Cid = cid.parse().map_err(anyhow::Error::from)?;
                async_block_unchecked(self.ipfs.remove_pin(&cid, false))?;
            }
            Err(_) => {}
        };

        let bytes = serde_json::to_vec(&identity)?;
        let ident_cid = async_block_unchecked(self.ipfs.put_dag(make_ipld!(bytes)))?;
        let root_handle =
            async_block_unchecked(self.ipfs.put_dag(make_ipld!({ "identity": ident_cid })))?;

        async_block_unchecked(self.ipfs.insert_pin(&root_handle, false))?;

        self.tesseract.set("root_cid", &root_handle.to_string())?;

        if let Ok(mut cache) = self.get_cache() {
            let mut query = QueryBuilder::default();
            //TODO: Query by public key to tie/assiociate the username to identity in the event of dup
            query.r#where("username", &old_identity.username())?;
            if let Ok(list) = cache.get_data(DataType::from(Module::Accounts), Some(&query)) {
                //get last
                if !list.is_empty() {
                    let mut obj = list.last().unwrap().clone();
                    obj.set_payload(identity.clone())?;
                    cache.add_data(DataType::from(Module::Accounts), &obj)?;
                }
            } else {
                cache.add_data(
                    DataType::from(Module::Accounts),
                    &DataObject::new(DataType::from(Module::Accounts), identity.clone())?,
                )?;
            }
        }

        //TODO: store and broadcast identity

        // if let Ok(hooks) = self.get_hooks() {
        //     let object = DataObject::new(DataType::Accounts, identity.clone())?;
        //     hooks.trigger("accounts::update_identity", &object);
        // }

        Ok(())
    }

    fn decrypt_private_key(&self, passphrase: Option<&str>) -> Result<Vec<u8>, Error> {
        self.raw_keypair()
            .map(|kp| kp.encode().to_vec())
            .map_err(Error::from)
    }

    fn refresh_cache(&mut self) -> Result<(), Error> {
        self.get_cache()?.empty(DataType::from(self.module()))
    }
}

impl Friends for IpfsIdentity {
    fn send_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }

    fn accept_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }

    fn deny_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }

    fn close_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }

    fn list_incoming_request(&self) -> Result<Vec<FriendRequest>, Error> {
        todo!()
    }

    fn list_outgoing_request(&self) -> Result<Vec<FriendRequest>, Error> {
        todo!()
    }

    fn list_all_request(&self) -> Result<Vec<FriendRequest>, Error> {
        todo!()
    }

    fn remove_friend(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }

    fn block_key(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }

    fn list_friends(&self) -> Result<Vec<Identity>, Error> {
        todo!()
    }

    fn has_friend(&self, pubkey: PublicKey) -> Result<(), Error> {
        todo!()
    }
}

#[allow(dead_code)]
fn to_ipld<S: serde::Serialize>(ser: S) -> anyhow::Result<Ipld> {
    let value = serde_json::to_value(ser)?;
    let item = match value {
        serde_json::Value::Null => Ipld::Null,
        serde_json::Value::Bool(bool) => Ipld::Bool(bool),
        //TODO: Maybe perform explicit check since all numbers are returned as Option::is_some
        //      otherwise this would continue to be null for a array of numbers
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(n), None, None) => Ipld::Integer(n as i128),
            (None, Some(n), None) => Ipld::Integer(n as i128),
            (None, None, Some(n)) => Ipld::Float(n),
            _ => Ipld::Null,
        },
        serde_json::Value::String(string) => Ipld::String(string),
        serde_json::Value::Array(arr) => {
            let mut ipld_arr = vec![];
            for item in arr {
                ipld_arr.push(to_ipld(item)?)
            }
            Ipld::List(ipld_arr)
        }
        serde_json::Value::Object(val_map) => {
            let mut map = BTreeMap::new();
            for (k, v) in val_map {
                let ipld = to_ipld(v)?;
                map.insert(k, ipld);
            }
            Ipld::Map(map)
        }
    };

    Ok(item)
}

#[allow(dead_code)]
fn from_ipld<D: DeserializeOwned>(ipld: &Ipld) -> anyhow::Result<D> {
    let value = match ipld {
        Ipld::Null => serde_json::Value::Null,
        Ipld::Bool(bool) => serde_json::Value::Bool(*bool),
        Ipld::Integer(i) => {
            if *i >= std::i64::MAX as i128 {
                //since we dont to convert i128 to i64 if its over the max we will return a null for now
                serde_json::Value::Null
            } else {
                let new_number = *i as i64;
                serde_json::Value::from(new_number)
            }
        }
        Ipld::Float(float) => serde_json::Value::from(*float),
        Ipld::String(string) => serde_json::Value::String(string.clone()),
        Ipld::Bytes(bytes) => serde_json::Value::from(bytes.clone()),
        Ipld::List(array) => {
            let mut value_arr = vec![];
            for item in array {
                let v = from_ipld(item)?;
                value_arr.push(v);
            }
            serde_json::Value::Array(value_arr)
        }
        Ipld::Map(map) => {
            let mut val_map = serde_json::Map::new();
            for (k, v) in map {
                let val = from_ipld(v)?;
                val_map.insert(k.clone(), val);
            }
            serde_json::Value::Object(val_map)
        }
        Ipld::Link(_) => serde_json::Value::Null, //Since "Value" doesnt have a cid link, we will leave this null for now
    };
    let item = serde_json::from_value(value)?;
    Ok(item)
}
