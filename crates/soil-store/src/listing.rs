//! The rich listing: the prose and artwork a storefront draws, as its own
//! content-addressed document.
//!
//! The manifest already carries a summary, categories, keywords, an icon and
//! screenshots, but a catalog row is addressed by a **listing CID** — one small
//! document a client fetches once per package and can cache forever, instead of
//! re-reading forty manifests or trusting a store's HTML. The same document is
//! what an agent reads: a listing is not a web page, it is data with pictures.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result, cid, reference};

pub const LISTING_SCHEMA: &str = "fungos.store.listing/1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub schema: String,
    pub spore_id: String,
    pub version: String,
    /// Package digest, so a listing can never drift onto another build.
    pub digest: String,
    pub name: String,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    pub license: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// `ipfs://` CID of the icon.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub screenshots: Vec<Shot>,
    /// The launcher monogram, so a client can draw a row before art resolves.
    #[serde(default)]
    pub badge: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub grants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    /// `ipfs://` CID of the image or clip. A path would put bytes back in the
    /// package, so it is refused where it is written.
    pub cid: String,
    pub caption: String,
}

impl Listing {
    /// Build a listing from a package tree and its current lock.
    ///
    /// The digest comes from the same `check` the installer uses, so a listing
    /// cannot describe a tree other than the one that would be installed.
    pub fn build(root: &Path) -> Result<Self> {
        let manifest = soil_spore::Manifest::load(root)?;
        let lock = soil_spore::check(root)?;
        let catalog = manifest.catalog.clone();
        let presentation = manifest.presentation.clone();
        let listing = Self {
            schema: LISTING_SCHEMA.into(),
            spore_id: manifest.spore.id.clone(),
            version: manifest.spore.version.clone(),
            digest: lock.digest.clone(),
            name: manifest.spore.name.clone(),
            summary: manifest.spore.summary.clone(),
            description: manifest.spore.description.clone(),
            license: manifest.spore.license.clone(),
            homepage: manifest.spore.homepage.clone(),
            categories: catalog
                .as_ref()
                .map(|value| value.categories.clone())
                .unwrap_or_default(),
            keywords: catalog
                .as_ref()
                .map(|value| value.keywords.clone())
                .unwrap_or_default(),
            icon: presentation.as_ref().and_then(|value| value.icon.clone()),
            screenshots: catalog
                .as_ref()
                .map(|value| {
                    value
                        .screenshots
                        .iter()
                        .map(|shot| Shot {
                            cid: shot.cid.clone(),
                            caption: shot.caption.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            badge: presentation.as_ref().map(|value| value.badge.clone()),
            color: presentation.as_ref().map(|value| value.color.clone()),
            system: manifest.system.as_ref().map(|value| value.role.clone()),
            grants: manifest.grants.keys().cloned().collect(),
        };
        listing.validate()?;
        Ok(listing)
    }

    /// Everything a client will draw from must be a CID, not a path.
    pub fn validate(&self) -> Result<()> {
        if self.schema != LISTING_SCHEMA {
            return Err(Error::Invalid(format!(
                "listing schema {:?} is not {LISTING_SCHEMA}",
                self.schema
            )));
        }
        if let Some(icon) = &self.icon
            && !reference(icon)
        {
            return Err(Error::Invalid(format!(
                "{}: icon is not an ipfs:// CID",
                self.spore_id
            )));
        }
        for shot in &self.screenshots {
            if !reference(&shot.cid) {
                return Err(Error::Invalid(format!(
                    "{}: screenshot {:?} is not an ipfs:// CID",
                    self.spore_id, shot.caption
                )));
            }
        }
        Ok(())
    }

    /// The listing's own address, over its canonical JSON bytes.
    pub fn cid(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|error| Error::Json(error.to_string()))?;
        Ok(cid::raw(&bytes))
    }

    pub fn parse(source: &str) -> Result<Self> {
        let listing: Self =
            serde_json::from_str(source).map_err(|error| Error::Json(error.to_string()))?;
        listing.validate()?;
        Ok(listing)
    }
}
