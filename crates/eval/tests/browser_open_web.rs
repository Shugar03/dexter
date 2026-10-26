use dexter_eval::{
    harvest::{HarvestGold, HarvestManifest},
    route_variant,
};
use std::path::PathBuf;

#[test]
fn browser_manifest_parses_and_reharvests_open_web_gold_labels() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/browser");
    let manifest_text =
        std::fs::read_to_string(root.join("manifest.toml")).expect("read browser manifest");
    let manifest: HarvestManifest = toml::from_str(&manifest_text).expect("parse browser manifest");
    let item_text =
        std::fs::read_to_string(root.join("items.jsonl")).expect("read browser snapshot");
    let items: Vec<serde_json::Value> = item_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .expect("parse browser snapshot JSON");

    let open_pages: Vec<_> = manifest
        .page
        .iter()
        .filter(|page| page.origin.as_deref() == Some("open-web"))
        .collect();
    let open_items: Vec<_> = items
        .iter()
        .filter(|item| item["meta"]["origin"].as_str() == Some("open-web"))
        .collect();
    assert_eq!(open_pages.len(), open_items.len());
    assert!(!open_pages.is_empty());

    for item in open_items {
        let id = item["id"].as_str().expect("item id");
        let page = manifest
            .page
            .iter()
            .find(|page| page.id == id)
            .expect("open-web item has a manifest page");
        assert_eq!(page.url.as_deref(), item["meta"]["url"].as_str());
        assert_eq!(page.goal, item["goal"].as_str().expect("item goal"));
        assert_eq!(page.settle_ms, Some(4000));

        match (&page.gold, item["gold"]["type"].as_str()) {
            (HarvestGold::Act { target }, Some("act")) => {
                assert_eq!(
                    target.role.as_deref(),
                    item["gold"]["target"]["role"].as_str(),
                    "role for {id}"
                );
                assert_eq!(
                    target.name.as_deref(),
                    item["gold"]["target"]["name"].as_str(),
                    "name for {id}"
                );
            }
            (HarvestGold::AnyOf { options }, Some("any_of")) => {
                let item_options = item["gold"]["options"].as_array().expect("item options");
                assert_eq!(options.len(), item_options.len(), "options for {id}");
                for (target, option) in options.iter().zip(item_options) {
                    assert_eq!(
                        target.role.as_deref(),
                        option["target"]["role"].as_str(),
                        "role for {id}"
                    );
                    assert_eq!(
                        target.name.as_deref(),
                        option["target"]["name"].as_str(),
                        "name for {id}"
                    );
                }
            }
            (HarvestGold::Route { route }, Some("route")) => {
                assert_eq!(
                    route_variant(route),
                    item["gold"]["route"]["type"].as_str().expect("route type"),
                    "route for {id}"
                );
            }
            _ => panic!("manifest gold differs from snapshot for {id}"),
        }
    }
}
