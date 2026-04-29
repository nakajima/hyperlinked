pub use crate::entity::app_kv::{
    delete, get, get_entry, get_many, set, set_entry, set_entry_with_updated_at,
};

#[cfg(test)]
#[path = "../../../tests/unit/app_models_kv_store.rs"]
mod tests;
