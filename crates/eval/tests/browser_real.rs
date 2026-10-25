use dexter_eval::{harvest::HarvestManifest, load_jsonl, Gold};
use std::path::PathBuf;

#[test]
fn browser_real_snapshot_loads_with_harvest_provenance_and_valid_act_elements() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/browser-real/items.jsonl");
    let text = std::fs::read_to_string(path).expect("read browser-real snapshot");
    let items = load_jsonl(&text).expect("parse browser-real snapshot");

    assert_eq!(items.len(), 17);
    for item in &items {
        assert_eq!(item.source, "harvest", "unexpected source on {}", item.id);
        if let Gold::Act { element, .. } = &item.gold {
            assert!(
                item.observation
                    .elements
                    .iter()
                    .any(|observed| observed.id == *element),
                "gold element {element} for {} is absent from its observation",
                item.id
            );
        }
    }
}

#[test]
fn browser_real_manifest_is_a_fifteen_page_reharvest_subset() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/browser-real/manifest.toml");
    let text = std::fs::read_to_string(path).expect("read browser-real re-harvest manifest");
    let manifest: HarvestManifest =
        toml::from_str(&text).expect("parse browser-real re-harvest manifest");

    assert_eq!(manifest.page.len(), 15);
    assert!(manifest
        .page
        .iter()
        .all(|page| page.settle_ms == Some(4000)));
    assert!(!manifest.page.iter().any(|page| {
        matches!(
            page.id.as_str(),
            "real-hn-hide-specific" | "real-bbc-save-specific"
        )
    }));
}
