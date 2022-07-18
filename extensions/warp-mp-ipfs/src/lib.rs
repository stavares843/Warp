// Used to ignore unused variables, mostly related to ones in the trait functions
//TODO: Remove
//TODO: Use rust-ipfs branch with major changes for pubsub, ipld, etc
#![allow(unused_variables)]
#![allow(unused_imports)]

pub mod config;
pub mod store;

use anyhow::bail;
use config::Config;
use futures::{Future, TryFutureExt};
use libipld::serde::to_ipld;
use libipld::{ipld, Cid, Ipld};
use serde::de::DeserializeOwned;
use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use store::friends::FriendsStore;
use store::identity::{IdentityStore, LookupBy};
use warp::data::{DataObject, DataType};
use warp::pocket_dimension::query::QueryBuilder;
use warp::sync::{Arc, Mutex, MutexGuard};

use warp::module::Module;
use warp::pocket_dimension::PocketDimension;
use warp::tesseract::Tesseract;
use warp::{async_block_in_place_uncheck, Extension, SingleHandle};

use ipfs::{
    Block, Ipfs, IpfsOptions, IpfsPath, Keypair, PeerId, Protocol, Types, UninitializedIpfs,
};
use tokio::sync::mpsc::Sender;
use warp::crypto::rand::Rng;
use warp::crypto::PublicKey;
use warp::error::Error;
use warp::multipass::generator::generate_name;
use warp::multipass::identity::{FriendRequest, Identifier, Identity, IdentityUpdate};
use warp::multipass::{identity, Friends, MultiPass};

#[derive(Clone)]
pub struct IpfsIdentity {
    path: PathBuf,
    cache: Option<Arc<Mutex<Box<dyn PocketDimension>>>>,
    tesseract: Tesseract,
    ipfs: Ipfs<Types>,
    temp: bool,
    friend_store: FriendsStore,
    identity_store: IdentityStore,
}

impl Drop for IpfsIdentity {
    fn drop(&mut self) {
        // We want to gracefully close the ipfs repo to allow for any cleanup
        async_block_in_place_uncheck(self.ipfs.clone().exit_daemon());

        // If IpfsIdentity::temporary was used, `temp` would be true and it would
        // let is to delete the repo
        if self.temp {
            if let Err(_e) = std::fs::remove_dir_all(&self.path) {}
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
            if config.path.is_some() {
                anyhow::bail!("Path cannot be set")
            }
        }
        IpfsIdentity::new(config.unwrap_or_default(), tesseract, cache).await
    }

    pub async fn persistent(
        config: Config,
        tesseract: Tesseract,
        cache: Option<Arc<Mutex<Box<dyn PocketDimension>>>>,
    ) -> anyhow::Result<IpfsIdentity> {
        if config.path.is_none() {
            anyhow::bail!("Path is required for identity to be persistent")
        }
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
            Err(_) => {
                let mut tesseract = tesseract.clone();
                if let Keypair::Ed25519(kp) = Keypair::generate_ed25519() {
                    let encoded_kp = bs58::encode(&kp.encode()).into_string();
                    tesseract.set("ipfs_keypair", &encoded_kp)?;
                    Keypair::Ed25519(kp)
                } else {
                    anyhow::bail!("Unreachable")
                }
            }
        };

        let temp = config.path.is_none();
        let path = config.path.unwrap_or_else(|| {
            let temp = warp::crypto::rand::thread_rng().gen_range(0, 1000);
            std::env::temp_dir().join(&format!("ipfs-temp-{temp}"))
        });

        let opts = IpfsOptions {
            ipfs_path: path.clone(),
            keypair: keypair.clone(),
            bootstrap: config.bootstrap,
            mdns: config.ipfs_setting.mdns.enable,
            kad_protocol: None,
            listening_addrs: config.listen_on,
            span: None,
            dcutr: config.ipfs_setting.dcutr.enable,
            relay: config.ipfs_setting.relay_client.enable,
            relay_server: config.ipfs_setting.relay_server.enable,
            relay_addr: config.ipfs_setting.relay_client.relay_address,
        };

        // Create directory if it doesnt exist
        if !opts.ipfs_path.exists() {
            tokio::fs::create_dir(opts.ipfs_path.clone()).await?;
        }

        let (ipfs, fut) = UninitializedIpfs::new(opts).start().await?;
        tokio::spawn(fut);

        let identity_store = IdentityStore::new(
            ipfs.clone(),
            tesseract.clone(),
            config.store_setting.discovery,
            config.store_setting.broadcast_interval,
        )
        .await?;

        let friend_store = FriendsStore::new(
            ipfs.clone(),
            tesseract.clone(),
            config.store_setting.discovery,
            config.store_setting.broadcast_interval,
        )
        .await?;

        let identity = IpfsIdentity {
            path,
            tesseract,
            cache,
            ipfs,
            temp,
            friend_store,
            identity_store,
        };

        Ok(identity)
    }

    pub fn get_cache(&self) -> anyhow::Result<MutexGuard<Box<dyn PocketDimension>>> {
        let cache = self
            .cache
            .as_ref()
            .ok_or(Error::PocketDimensionExtensionUnavailable)?;

        Ok(cache.lock())
    }
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
        let identity = async_block_in_place_uncheck(self.identity_store.create_identity(username))?;

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
                self.identity_store.lookup(LookupBy::PublicKey(pk))
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
                self.identity_store.lookup(LookupBy::Username(username))
            }
            (None, None, true) => {
                return async_block_in_place_uncheck(self.identity_store.own_identity())
            }
            _ => Err(Error::InvalidIdentifierCondition),
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

        if let Ok(cid) = self.tesseract.retrieve("ident_cid") {
            let cid: Cid = cid.parse().map_err(anyhow::Error::from)?;
            async_block_in_place_uncheck(self.ipfs.remove_pin(&cid, false))?;
        };

        let ipld = to_ipld(&identity).map_err(anyhow::Error::from)?;
        let ident_cid = async_block_in_place_uncheck(self.ipfs.put_dag(ipld))?;

        async_block_in_place_uncheck(self.ipfs.insert_pin(&ident_cid, false))?;

        self.tesseract.set("ident_cid", &ident_cid.to_string())?;

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

        async_block_in_place_uncheck(self.identity_store.update_identity())?;

        // if let Ok(hooks) = self.get_hooks() {
        //     let object = DataObject::new(DataType::Accounts, identity.clone())?;
        //     hooks.trigger("accounts::update_identity", &object);
        // }

        Ok(())
    }

    fn decrypt_private_key(&self, passphrase: Option<&str>) -> Result<Vec<u8>, Error> {
        self.identity_store
            .get_raw_keypair()
            .map(|kp| kp.encode().to_vec())
            .map_err(Error::from)
    }

    fn refresh_cache(&mut self) -> Result<(), Error> {
        self.get_cache()?.empty(DataType::from(self.module()))
    }
}

impl Friends for IpfsIdentity {
    fn send_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        async_block_in_place_uncheck(self.friend_store.send_request(pubkey))
    }

    fn accept_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        async_block_in_place_uncheck(self.friend_store.accept_request(pubkey))
    }

    fn deny_request(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        async_block_in_place_uncheck(self.friend_store.reject_request(pubkey))
    }

    fn list_incoming_request(&self) -> Result<Vec<FriendRequest>, Error> {
        Ok(self.friend_store.list_incoming_request())
    }

    fn list_outgoing_request(&self) -> Result<Vec<FriendRequest>, Error> {
        Ok(self.friend_store.list_outgoing_request())
    }

    fn list_all_request(&self) -> Result<Vec<FriendRequest>, Error> {
        Ok(self.friend_store.list_all_request())
    }

    fn remove_friend(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        async_block_in_place_uncheck(self.friend_store.remove_friend(pubkey))
    }

    fn block(&mut self, pubkey: PublicKey) -> Result<(), Error> {
        async_block_in_place_uncheck(self.friend_store.block(pubkey))
    }

    fn block_list(&self) -> Result<Vec<PublicKey>, Error> {
        async_block_in_place_uncheck(self.friend_store.block_list())
    }

    fn list_friends(&self) -> Result<Vec<PublicKey>, Error> {
        async_block_in_place_uncheck(self.friend_store.friends_list())
    }

    fn has_friend(&self, pubkey: PublicKey) -> Result<(), Error> {
        let list = self.list_friends()?;
        for pk in list {
            if pk == pubkey {
                return Ok(());
            }
        }
        Err(Error::FriendDoesntExist)
    }
}

pub mod ffi {
    use crate::config::Config;
    use crate::IpfsIdentity;
    use std::ffi::CStr;
    use std::os::raw::c_char;
    use warp::error::Error;
    use warp::ffi::FFIResult;
    use warp::multipass::MultiPassAdapter;
    use warp::pocket_dimension::PocketDimensionAdapter;
    use warp::sync::{Arc, Mutex};
    use warp::tesseract::Tesseract;
    use warp::{async_on_block, runtime_handle};

    #[allow(clippy::missing_safety_doc)]
    #[no_mangle]
    pub unsafe extern "C" fn multipass_mp_ipfs_temporary(
        pocketdimension: *const PocketDimensionAdapter,
        tesseract: *const Tesseract,
        config: *const c_char,
    ) -> FFIResult<MultiPassAdapter> {
        let tesseract = match tesseract.is_null() {
            false => {
                let tesseract = &*tesseract;
                tesseract.clone()
            }
            true => Tesseract::default(),
        };

        let config = match config.is_null() {
            true => None,
            false => {
                let config = CStr::from_ptr(config).to_string_lossy().to_string();
                match serde_json::from_str(&config) {
                    Ok(c) => Some(c),
                    Err(e) => return FFIResult::err(Error::from(e)),
                }
            }
        };

        let cache = match pocketdimension.is_null() {
            true => None,
            false => Some(&*pocketdimension),
        };

        let account = match async_on_block(IpfsIdentity::temporary(
            config,
            tesseract,
            cache.map(|c| c.inner()),
        )) {
            Ok(identity) => identity,
            Err(e) => return FFIResult::err(Error::from(e)),
        };

        FFIResult::ok(MultiPassAdapter::new(Arc::new(Mutex::new(Box::new(
            account,
        )))))
    }

    #[allow(clippy::missing_safety_doc)]
    #[no_mangle]
    pub unsafe extern "C" fn multipass_mp_ipfs_persistent(
        pocketdimension: *const PocketDimensionAdapter,
        tesseract: *const Tesseract,
        config: *const c_char,
    ) -> FFIResult<MultiPassAdapter> {
        let tesseract = match tesseract.is_null() {
            false => {
                let tesseract = &*tesseract;
                tesseract.clone()
            }
            true => Tesseract::default(),
        };

        let config = match config.is_null() {
            true => {
                return FFIResult::err(Error::from(anyhow::anyhow!("Configuration is invalid")))
            }
            false => {
                let config = CStr::from_ptr(config).to_string_lossy().to_string();
                match serde_json::from_str(&config) {
                    Ok(c) => c,
                    Err(e) => return FFIResult::err(Error::from(e)),
                }
            }
        };

        let cache = match pocketdimension.is_null() {
            true => None,
            false => Some(&*pocketdimension),
        };

        let account = match async_on_block(IpfsIdentity::persistent(
            config,
            tesseract,
            cache.map(|c| c.inner()),
        )) {
            Ok(identity) => identity,
            Err(e) => return FFIResult::err(Error::from(e)),
        };

        FFIResult::ok(MultiPassAdapter::new(Arc::new(Mutex::new(Box::new(
            account,
        )))))
    }
}
