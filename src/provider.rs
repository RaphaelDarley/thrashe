use std::sync::RwLock;

use crate::thrashe::{CacheSpec, CacheState, Thrashe, ThrasheReport};

// #[cfg(feature = "convenience_types")]
// use paste::paste;

pub trait CacheProvider {
    fn get_cache() -> &'static RwLock<Option<CacheState>>;

    fn configure(spec: CacheSpec) -> Option<ThrasheReport> {
        let state = Self::get_cache();
        state
            .write()
            .unwrap()
            .replace(CacheState::from_spec(spec))
            .map(|s| s.make_report())
    }

    fn get_report() -> Option<ThrasheReport> {
        let state = Self::get_cache();
        state.read().unwrap().as_ref().map(|s| s.make_report())
    }

    fn finish() -> Option<ThrasheReport> {
        let state = Self::get_cache();
        state.write().unwrap().take().map(|s| s.make_report())
    }
}

#[macro_export]
macro_rules! new_provider {
    ($name: ident) => {
        pub enum $name {}

        new_type($name);

        impl CacheProvider for $name {
            fn get_cache() -> &'static RwLock<Option<CacheState>> {
                static STATE: RwLock<Option<CacheState>> = RwLock::new(None);
                &STATE
            }
        }
    };
}

#[cfg(feature = "convenience_types")]
macro_rules! new_type {
    ($name: ident) => {
        paste! {
            pub type [<Thrashe $name>]<T> = Thrashe<T, $name>;
        }
    };
}

#[cfg(not(feature = "convenience_types"))]
macro_rules! new_type {
    ($name: ident) => {};
}

// pub struct GlobalCache;

// impl CacheProvider for GlobalCache {
//     fn get_cache() -> &'static RwLock<Option<CacheState>> {
//         static STATE: RwLock<Option<CacheState>> = RwLock::new(None);
//         &STATE
//     }
// }

pub enum GlobalCache {}

impl CacheProvider for GlobalCache {
    fn get_cache() -> &'static RwLock<Option<CacheState>> {
        static STATE: RwLock<Option<CacheState>> = RwLock::new(None);
        &STATE
    }
}
