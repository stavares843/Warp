pub mod error;

pub use warp_module::Module;

#[cfg(not(target_os = "wasm32"))]
pub use anyhow;
#[cfg(feature = "bincode_opt")]
#[cfg(not(target_os = "wasm32"))]
pub use bincode;
pub use cfg_if;
pub use chrono;
#[cfg(not(any(target_os = "android", target_os = "ios", target_family = "wasm")))]
pub use dirs;
pub use regex;
pub use serde;
pub use serde_json;
pub use serde_yaml;
pub use toml;
pub use uuid;

#[cfg(feature = "async")]
#[cfg(not(target_family = "wasm"))]
pub use tokio;

#[cfg(feature = "async")]
#[cfg(not(target_family = "wasm"))]
pub use tokio_stream;

#[cfg(feature = "async")]
#[cfg(not(target_family = "wasm"))]
pub use tokio_util;

#[cfg(feature = "async")]
#[cfg(not(target_family = "wasm"))]
pub use async_trait;

#[cfg(feature = "async")]
#[cfg(not(target_family = "wasm"))]
pub use futures;

#[cfg(not(target_family = "wasm"))]
pub type Result<T> = std::result::Result<T, crate::error::Error>;

#[cfg(target_os = "wasm32")]
pub mod wasm {
    use wasm_bindgen::JsError;

    pub type Result<T> = std::result::Result<T, JsError>;
}

#[cfg(target_os = "wasm32")]
pub use wasm::Result;

/// Functions that provide information about extensions that iterates around a Module
pub trait Extension {
    /// Returns an id of the extension. Should be the crate name (eg in a `warp-module-ext` format)
    fn id(&self) -> String {
        self.name()
    }

    /// Returns the name of an extension
    fn name(&self) -> String;

    /// Returns the description of the extension
    fn description(&self) -> String {
        format!(
            "{} is an extension that is designed to be used for {}",
            self.name(),
            self.module()
        )
    }

    /// Returns the module type the extension is meant to be used for
    fn module(&self) -> Module;
}
