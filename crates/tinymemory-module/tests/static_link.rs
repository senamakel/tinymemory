//! Public linked-module entry points remain callable by a Rust host.

#![cfg(feature = "static-link")]

use tinybus::module::abi::{TbModuleInit, TbSlice, ABI_MAGIC};
use tinymemory_module::{
    tinybus_module_init_v1, tinybus_module_manifest_v1, TINYBUS_MODULE_ABI_V1,
};

#[test]
fn linked_module_exposes_its_descriptor_manifest_and_initializer() {
    assert_eq!(TINYBUS_MODULE_ABI_V1.magic, ABI_MAGIC);
    let manifest: extern "C" fn() -> TbSlice = tinybus_module_manifest_v1;
    let slice = manifest();
    assert!(!slice.ptr.is_null());
    assert!(slice.len > 0);
    let initialize: TbModuleInit = tinybus_module_init_v1;
    assert_ne!(initialize as usize, 0);
}
