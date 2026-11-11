//! A listing is the storefront's unit: prose and artwork, addressed by one CID.

use std::path::Path;

use soil_store::{Listing, media};

const ICON: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
const SHOT: &str = "ipfs://bafybeieq3lqk4hbgvfxruhzr5sgl3qovuyz4rjps2q7dugxn6mx6c5kpcy";

fn tree(dir: &Path, extra: &str) {
    std::fs::create_dir_all(dir.join("ui")).expect("ui");
    std::fs::write(
        dir.join("spore.toml"),
        format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.demo\"\nname = \"demo\"\nversion = \"0.1.0\"\n\
             license = \"MIT\"\nsummary = \"A demo\"\nhomepage = \"https://example.invalid\"\n\
             [engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n\
             [ui]\nentry = \"ui/main.lua\"\n[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n{extra}"
        ),
    )
    .expect("manifest");
    std::fs::write(dir.join("ui/main.lua"), "return {}").expect("view");
}

/// The presentation and grants a listing draws from.
fn art() -> String {
    format!(
        "[presentation]\nbadge = \"D\"\ncolor = \"#8b9dff\"\nicon = \"{ICON}\"\n\
         [grants]\n\"soil:canvas\" = \"present the demo\"\n"
    )
}

#[test]
fn a_listing_is_one_address_for_prose_and_art() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    tree(
        dir,
        &format!(
            "{}[catalog]\ncategories = [\"developer-tools\"]\nkeywords = [\"demo\", \"store\"]\n\
             screenshots = [\n  {{ cid = \"{SHOT}\", caption = \"The demo running\" }},\n]\n",
            art()
        ),
    );

    let listing = Listing::build(dir).expect("build a listing");
    assert_eq!(listing.spore_id, "dev.fungos.demo");
    assert_eq!(listing.categories, vec!["developer-tools"]);
    assert_eq!(listing.icon.as_deref(), Some(ICON));
    assert_eq!(listing.screenshots.len(), 1);
    assert_eq!(listing.screenshots[0].caption, "The demo running");
    assert_eq!(listing.badge.as_deref(), Some("D"));
    assert_eq!(listing.grants, vec!["soil:canvas"]);
    assert_eq!(listing.digest.len(), 64, "a listing names one build");

    let address = listing.cid().expect("address");
    assert_eq!(address, listing.cid().expect("address"), "stable");

    // The listing's own bytes live in the same content-addressed directory as
    // the art it points at, so a client resolves both the same way.
    let bytes = serde_json::to_vec(&listing).expect("json");
    let stored = media::add(dir, &bytes).expect("store the listing");
    assert_eq!(stored, address);
    let back = media::get(dir, &address).expect("read it back");
    assert_eq!(
        Listing::parse(&String::from_utf8(back).unwrap()).unwrap(),
        listing
    );
}

#[test]
fn a_path_where_a_cid_belongs_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    tree(
        dir,
        &format!(
            "{}[catalog]\ncategories = [\"utilities\"]\n\
             screenshots = [{{ cid = \"art/shot.png\", caption = \"a local file\" }}]\n",
            art()
        ),
    );
    let error = Listing::build(dir).expect_err("a path is not a CID");
    assert!(error.to_string().contains("ipfs://"), "{error}");
}
