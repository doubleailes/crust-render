use crate::hittable::Hittable;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub static GLOBAL_OBJ_CACHE: Lazy<RwLock<HashMap<String, Arc<dyn Hittable>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
