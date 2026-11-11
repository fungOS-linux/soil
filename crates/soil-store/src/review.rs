//! Reviews and ratings: claims about one build, stored by the store operator.
//!
//! A review is bound to a **digest**, never to a name alone: a review of
//! `0.1.0` must not silently apply to `0.2.0`, whose bytes nobody ran. The
//! operator keeps the file — it is the store's moderation surface, not the
//! package's — and hiding is a record with a reason, so the aggregate can change
//! without the history disappearing.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result, digest};

pub const SCHEMA: u32 = 1;
/// Reviews needed before a rating is more than its author's opinion.
const PRIOR_WEIGHT: f64 = 20.0;
/// What the prior is worth when the store has no history at all.
const PRIOR_MEAN: f64 = 3.0;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub spore_id: String,
    pub version: String,
    /// The build this is about.
    pub digest: String,
    pub principal: String,
    /// 1..=5.
    pub rating: u8,
    /// May be empty: a rating is a review without prose.
    #[serde(default)]
    pub body: String,
    pub created_at: u64,
}

/// A moderation decision. The review stays in the file; it stops counting.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hidden {
    pub digest: String,
    pub principal: String,
    pub reason: String,
    pub hidden_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reviews {
    pub schema: u32,
    #[serde(default)]
    pub reviews: Vec<Review>,
    #[serde(default)]
    pub hidden: Vec<Hidden>,
}

/// One aggregate, as a reader sees it.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rating {
    pub count: u64,
    /// The plain mean, for transparency.
    pub mean: f64,
    /// The mean with the store's prior, which is what a ranking uses.
    pub weighted: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Aggregate {
    /// Reviews of exactly this digest.
    pub current: Rating,
    /// Reviews of every version of this Spore.
    pub all: Rating,
}

impl Default for Reviews {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            reviews: Vec::new(),
            hidden: Vec::new(),
        }
    }
}

impl Reviews {
    pub fn read(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let source = std::fs::read_to_string(path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        let reviews: Self =
            serde_json::from_str(&source).map_err(|error| Error::Json(error.to_string()))?;
        if reviews.schema != SCHEMA {
            return Err(Error::Invalid(format!(
                "reviews schema {} is not {SCHEMA}",
                reviews.schema
            )));
        }
        Ok(reviews)
    }

    /// Write through a temporary file, so a killed process leaves the previous
    /// file rather than half of the next one.
    pub fn write(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).map_err(|error| Error::Json(error.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, format!("{json}\n"))
            .map_err(|error| Error::Io(format!("{}: {error}", temporary.display())))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))
    }

    /// Append one review. The newest review by a principal for a digest is the
    /// current one, so an edit is an append that supersedes.
    pub fn add(&mut self, review: Review) -> Result<()> {
        review.validate()?;
        self.reviews.push(review);
        Ok(())
    }

    /// Hide a principal's current review of one digest.
    pub fn hide(&mut self, hidden: Hidden) -> Result<()> {
        if hidden.reason.trim().is_empty() {
            return Err(Error::Invalid("a hide needs a reason".into()));
        }
        if !digest(&hidden.digest) {
            return Err(Error::Invalid("hide names no digest".into()));
        }
        if !self
            .reviews
            .iter()
            .any(|review| review.digest == hidden.digest && review.principal == hidden.principal)
        {
            return Err(Error::Invalid(format!(
                "no review by {} of {}",
                hidden.principal, hidden.digest
            )));
        }
        self.hidden.push(hidden);
        Ok(())
    }

    /// The newest review per principal for one digest, hidden ones removed.
    pub fn visible(&self, requested: &str) -> Vec<&Review> {
        let mut latest: Vec<&Review> = Vec::new();
        for review in &self.reviews {
            if review.digest != requested {
                continue;
            }
            match latest
                .iter_mut()
                .find(|kept| kept.principal == review.principal)
            {
                Some(kept) => *kept = review,
                None => latest.push(review),
            }
        }
        latest.retain(|review| {
            !self.hidden.iter().any(|hidden| {
                hidden.digest == review.digest && hidden.principal == review.principal
            })
        });
        latest.sort_by_key(|review| std::cmp::Reverse(review.created_at));
        latest
    }

    pub fn aggregate(&self, requested: &str, spore_id: &str) -> Aggregate {
        let visible = self.visible(requested);
        let prior = self.prior(requested);
        let current = rating(visible.iter().map(|review| review.rating), prior);
        let all = rating(
            self.reviews
                .iter()
                .filter(|review| {
                    review.spore_id == spore_id
                        && !self.hidden.iter().any(|hidden| {
                            hidden.digest == review.digest && hidden.principal == review.principal
                        })
                })
                .map(|review| review.rating),
            prior,
        );
        Aggregate { current, all }
    }

    /// The mean of every review of *another* build, which is the prior's centre.
    ///
    /// A build's own ratings must not set the centre they are pulled toward:
    /// with one five-star review and nothing else, the honest answer is "one
    /// person liked it", not "this build is perfect".
    fn prior(&self, requested: &str) -> f64 {
        let (sum, count) = self
            .reviews
            .iter()
            .filter(|review| review.digest != requested)
            .fold((0.0, 0_u64), |(sum, count), review| {
                (sum + f64::from(review.rating), count + 1)
            });
        if count == 0 {
            PRIOR_MEAN
        } else {
            sum / count as f64
        }
    }
}

impl Review {
    pub fn validate(&self) -> Result<()> {
        if !(1..=5).contains(&self.rating) {
            return Err(Error::Invalid(format!(
                "rating {} is not 1..=5",
                self.rating
            )));
        }
        if self.principal.trim().is_empty() {
            return Err(Error::Invalid("a review needs a principal".into()));
        }
        if self.spore_id.trim().is_empty() || self.version.trim().is_empty() {
            return Err(Error::Invalid(
                "a review needs a Spore and a version".into(),
            ));
        }
        if !digest(&self.digest) {
            return Err(Error::Invalid(
                "a review is about one package digest".into(),
            ));
        }
        Ok(())
    }
}

/// Mean and prior-weighted mean over a set of ratings.
fn rating(values: impl Iterator<Item = u8>, prior: f64) -> Rating {
    let (sum, count) = values.fold((0.0, 0_u64), |(sum, count), value| {
        (sum + f64::from(value), count + 1)
    });
    if count == 0 {
        return Rating {
            count: 0,
            mean: 0.0,
            weighted: 0.0,
        };
    }
    Rating {
        count,
        mean: sum / count as f64,
        weighted: (sum + PRIOR_WEIGHT * prior) / (count as f64 + PRIOR_WEIGHT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const OTHER: &str = "0f0e0d0c0b0a090807060504030201000f0e0d0c0b0a09080706050403020100";

    fn review(principal: &str, digest: &str, rating: u8, at: u64) -> Review {
        Review {
            spore_id: "dev.fungos.demo".into(),
            version: "0.1.0".into(),
            digest: digest.into(),
            principal: principal.into(),
            rating,
            body: String::new(),
            created_at: at,
        }
    }

    #[test]
    fn an_edit_supersedes_and_a_hide_stops_it_counting() {
        let mut reviews = Reviews::default();
        reviews.add(review("alice", DIGEST, 2, 10)).unwrap();
        reviews.add(review("bob", DIGEST, 4, 11)).unwrap();
        assert_eq!(reviews.visible(DIGEST).len(), 2);
        assert_eq!(
            reviews.aggregate(DIGEST, "dev.fungos.demo").current.count,
            2
        );

        // An edit is a new record: history stays, the aggregate follows the latest.
        reviews.add(review("alice", DIGEST, 5, 12)).unwrap();
        let visible = reviews.visible(DIGEST);
        assert_eq!(visible.len(), 2);
        let alice = visible
            .iter()
            .find(|review| review.principal == "alice")
            .unwrap();
        assert_eq!(alice.rating, 5);
        assert_eq!(alice.created_at, 12);

        reviews
            .hide(Hidden {
                digest: DIGEST.into(),
                principal: "bob".into(),
                reason: "not about this build".into(),
                hidden_at: 13,
            })
            .unwrap();
        let aggregate = reviews.aggregate(DIGEST, "dev.fungos.demo");
        assert_eq!(aggregate.current.count, 1, "a hidden review stops counting");
        assert_eq!(reviews.reviews.len(), 3, "and stays in the file");
    }

    #[test]
    fn a_rating_is_pulled_toward_the_stores_prior() {
        let mut reviews = Reviews::default();
        reviews.add(review("alice", DIGEST, 5, 10)).unwrap();
        let aggregate = reviews.aggregate(DIGEST, "dev.fungos.demo");
        assert_eq!(aggregate.current.count, 1);
        assert_eq!(aggregate.current.mean, 5.0);
        assert!(
            aggregate.current.weighted < 4.0,
            "one five-star review must not outrank a history: {}",
            aggregate.current.weighted
        );
    }

    #[test]
    fn a_review_is_about_one_build() {
        let mut reviews = Reviews::default();
        reviews.add(review("alice", DIGEST, 4, 10)).unwrap();
        reviews.add(review("alice", OTHER, 1, 11)).unwrap();
        assert_eq!(
            reviews.aggregate(DIGEST, "dev.fungos.demo").current.count,
            1
        );
        assert_eq!(reviews.aggregate(OTHER, "dev.fungos.demo").current.count, 1);
        assert_eq!(
            reviews.aggregate(DIGEST, "dev.fungos.demo").all.count,
            2,
            "all-versions is labelled separately"
        );
        assert!(reviews.add(review("carol", "not-a-digest", 3, 12)).is_err());
        assert!(reviews.add(review("carol", DIGEST, 6, 12)).is_err());
    }
}
