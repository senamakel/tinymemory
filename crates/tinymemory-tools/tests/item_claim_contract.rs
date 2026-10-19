//! Compile contract for custom list-store claim implementations.

use tinymemory_tools::list_store::{ItemClaim, ListStore, StoreError};

#[derive(Clone)]
struct Item;

struct ExternalStore;

impl ListStore<Item> for ExternalStore {
    fn load(&self) -> Result<Vec<Item>, StoreError> {
        Ok(Vec::new())
    }

    fn save(&self, _entries: &[Item]) -> Result<(), StoreError> {
        Ok(())
    }

    fn claim_oldest(
        &self,
        _excluded: &[String],
        _eligible_at_ms: i64,
        _id_of: &dyn Fn(&Item) -> String,
    ) -> Result<Option<ItemClaim<Item>>, StoreError> {
        Ok(Some(ItemClaim::new(
            "external-id".into(),
            Item,
            7,
            11,
            "external-holder".into(),
        )))
    }
}

#[test]
fn external_list_store_can_construct_and_validate_its_claim_token() {
    let claim = ExternalStore
        .claim_oldest(&[], i64::MAX, &|_| "external-id".into())
        .unwrap()
        .unwrap();

    assert_eq!(claim.id, "external-id");
    assert_eq!(claim.version, 7);
    assert_eq!(claim.epoch, 11);
    assert_eq!(claim.holder, "external-holder");
}
