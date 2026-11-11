//! Desktops: the unit of work, sharing and installation.
//!
//! Three nouns and one rule. **Principal** is who — and it is deliberately just
//! the `actor` string `soil:principal` already resolves, so nothing here
//! decides identity. **Device** is a machine, owned by one principal and
//! possibly assigned to another. **Desktop** is the unit of work and sharing: a
//! VFS export, a set of members, a set of installs, and the capabilities the
//! people in it may reach.
//!
//! The rule is that a *relation to a desktop* is not the same as *access to an
//! object inside it*. Sharing one document with a guest must not quietly make
//! them a member, and membership must not be the only way to receive a file.
//! Keeping those two mechanisms apart is what lets both exist.
//!
//! This crate is the model and nothing else: no I/O, no mounts, no network. It
//! exists so the rules can be argued with in tests before anything is built on
//! them.
//!
//! Two deliberate absences, both worth knowing before extending it:
//!
//! - **No fleet noun.** A fleet is the set of devices that can reach a desktop,
//!   derived from relations, never stored. A stored one would be a second
//!   membership system to keep in step with this one.
//! - **No "global install".** An install is declared by a desktop and
//!   materialised by a device. A device-level install every desktop can see
//!   would be an invisible superset, and a shared desktop at someone's employer
//!   would see what they installed at home.

use std::collections::{BTreeMap, BTreeSet};

/// Who. Never more than the identity `soil:principal` resolved.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PrincipalId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// A capability a device can provide or a Spore can ask for, e.g. `soil:audio`.
///
/// A newtype rather than an enum so a new capability is a new string, not a
/// breaking change to every match in the tree.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Capability(String);

impl Capability {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Capability {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

/// What a relation permits inside a desktop.
///
/// Ordered so a stronger role satisfies a weaker requirement without a match
/// table: `Writer >= Reader`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Reader,
    Writer,
    Owner,
}

/// Why something was refused.
///
/// One enum for relations, shares and capabilities because a caller needs to
/// tell "you are not allowed" from "nobody offers this" — and because a refusal
/// with a reason is what a Spore can show a human.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No live relation to this desktop.
    NotAMember,
    /// The relation or grant does not cover this.
    NotGranted,
    /// Nobody in this desktop offers the capability.
    NoProvider,
    /// A relation existed and has run out.
    Expired,
    /// This desktop refuses this Spore outright.
    Denied,
}

/// A machine.
///
/// Owned by one principal, possibly assigned to another: an employer's laptop
/// is *owned* by the employer and *used* by a person, and both statements have
/// to be able to be true at once.
#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub owner: PrincipalId,
    pub assigned_to: Option<PrincipalId>,
    /// What this machine can provide itself.
    pub capabilities: BTreeSet<Capability>,
}

/// A principal and what they may do, until when.
#[derive(Clone, Debug)]
pub struct Membership {
    pub principal: PrincipalId,
    pub role: Role,
    pub expires_at_ms: Option<i64>,
}

/// One object shared with someone who is not a member.
///
/// `path` is inside the desktop's export. A share is read-only by design for
/// now: writing through one needs *path-scoped* sync, which does not exist yet,
/// and pretending otherwise would be a promise the protocol cannot keep.
#[derive(Clone, Debug)]
pub struct Share {
    pub path: String,
    pub grantee: PrincipalId,
    pub role: Role,
    pub expires_at_ms: Option<i64>,
}

/// What a desktop wants installed, and whether it insists.
///
/// `required` is the whole point of a desktop having an install list: a shared
/// desktop can pin a Spore and its grants, and a member that cannot satisfy the
/// pin is visibly non-compliant rather than quietly different.
#[derive(Clone, Debug)]
pub struct Install {
    pub spore: String,
    /// The version or digest the desktop will accept, when it pins one.
    pub version: Option<String>,
    pub grants: BTreeMap<String, String>,
    pub required: bool,
}

/// The unit of work and sharing.
#[derive(Clone, Debug)]
pub struct Desktop {
    pub id: String,
    pub owner: PrincipalId,
    pub members: Vec<Membership>,
    pub shares: Vec<Share>,
    pub installs: Vec<Install>,
    /// Spores this desktop refuses, whatever a device has installed.
    ///
    /// The mirror of `required`: a desktop that can insist on one Spore should
    /// be able to refuse another, or "shared desktop" means only "shared
    /// installs". `spore.toml` already carries a `denied` map for capabilities;
    /// this is the same intent one level up.
    pub denied: BTreeSet<String>,
}

/// A capability offered by a machine, for a desktop that admitted it.
#[derive(Clone, Debug)]
pub struct Provider {
    pub device: String,
    pub capability: Capability,
}

/// Where a capability came from, or why it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The host implements the interface itself: a function call, no boundary.
    InProcess,
    /// Another process on this machine owns the resource.
    Local {
        device: String,
    },
    /// A machine elsewhere in the desktop owns the resource.
    Peer {
        device: String,
    },
    Refused(Refusal),
}

impl Desktop {
    /// The relation this principal has to the desktop, if any.
    ///
    /// A share deliberately does not appear here: receiving a document is not
    /// membership, and a guest must not be able to read the rest of the tree by
    /// virtue of having been sent one file.
    pub fn relation(&self, principal: &PrincipalId, now_ms: i64) -> Result<Role, Refusal> {
        if &self.owner == principal {
            return Ok(Role::Owner);
        }
        let mut expired = false;
        let mut best: Option<Role> = None;
        for member in &self.members {
            if &member.principal != principal {
                continue;
            }
            match member.expires_at_ms {
                Some(at) if at <= now_ms => expired = true,
                _ => best = Some(best.map_or(member.role, |role| role.max(member.role))),
            }
        }
        best.ok_or(if expired {
            Refusal::Expired
        } else {
            Refusal::NotAMember
        })
    }

    /// Required installs this device does not have.
    #[must_use]
    pub fn unsatisfied(&self, installed: &BTreeSet<String>) -> Vec<&Install> {
        self.installs
            .iter()
            .filter(|install| install.required && !installed.contains(&install.spore))
            .collect()
    }

    /// Whether this desktop lets the Spore run at all.
    ///
    /// Denial is checked before any install state, so installing a refused
    /// Spore is not a way to run it. This is the one rule that has to hold
    /// against a device that has been tampered with, and it is why the answer
    /// is a refusal rather than a boolean the caller can forget to check.
    pub fn permits(&self, spore: &str) -> Result<(), Refusal> {
        if self.denied.contains(spore) {
            return Err(Refusal::Denied);
        }
        Ok(())
    }

    /// Spores this desktop both requires and refuses.
    ///
    /// Not an error the model can throw on its own — a desktop is configured by
    /// people — but a contradiction it can *name*, so a host can refuse to load
    /// it instead of silently letting denial win and leaving a required install
    /// permanently unsatisfiable.
    #[must_use]
    pub fn contradictions(&self) -> Vec<&str> {
        self.installs
            .iter()
            .filter(|install| install.required && self.denied.contains(&install.spore))
            .map(|install| install.spore.as_str())
            .collect()
    }
}

impl Share {
    /// Whether this share grants the principal anything, right now.
    pub fn authorize(&self, principal: &PrincipalId, now_ms: i64) -> Result<Role, Refusal> {
        if &self.grantee != principal {
            return Err(Refusal::NotGranted);
        }
        match self.expires_at_ms {
            Some(at) if at <= now_ms => Err(Refusal::Expired),
            _ => Ok(self.role),
        }
    }
}

/// Resolve a capability request, first match wins.
///
/// The order is cheapest-and-narrowest first: the host's own implementation,
/// then a provider on this machine, then one elsewhere in the desktop.
///
/// The important property is that **this machine is not a shortcut**. A local
/// provider is a remote that happens to be nearby: it is reached over a
/// provider protocol by a different process, so it passes the same relation
/// check as a peer would. If locality short-circuited authorization, moving a
/// provider onto this box would silently escalate it.
#[must_use]
pub fn resolve_capability(
    desktop: &Desktop,
    principal: &PrincipalId,
    device: &Device,
    capability: &Capability,
    providers: &[Provider],
    host: &BTreeSet<Capability>,
    now_ms: i64,
) -> Resolution {
    if let Err(refusal) = desktop.relation(principal, now_ms) {
        return Resolution::Refused(refusal);
    }
    if host.contains(capability) {
        return Resolution::InProcess;
    }
    if device.capabilities.contains(capability) {
        return Resolution::Local {
            device: device.id.clone(),
        };
    }
    match providers
        .iter()
        .find(|provider| &provider.capability == capability)
    {
        Some(provider) => Resolution::Peer {
            device: provider.device.clone(),
        },
        None => Resolution::Refused(Refusal::NoProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn principal(id: &str) -> PrincipalId {
        PrincipalId::from(id)
    }

    fn device(id: &str, owner: &str, assigned: Option<&str>, caps: &[&str]) -> Device {
        Device {
            id: id.to_owned(),
            owner: principal(owner),
            assigned_to: assigned.map(principal),
            capabilities: caps.iter().map(|cap| Capability::from(*cap)).collect(),
        }
    }

    fn desktop(owner: &str, members: &[(&str, Role)]) -> Desktop {
        Desktop {
            id: "fpl".to_owned(),
            owner: principal(owner),
            members: members
                .iter()
                .map(|(who, role)| Membership {
                    principal: principal(who),
                    role: *role,
                    expires_at_ms: None,
                })
                .collect(),
            shares: Vec::new(),
            installs: Vec::new(),
            denied: BTreeSet::new(),
        }
    }

    #[test]
    fn a_laptop_can_be_owned_by_one_principal_and_used_by_another() {
        let laptop = device("laptop", "employer", Some("avery"), &[]);
        assert_eq!(laptop.owner, principal("employer"));
        assert_eq!(laptop.assigned_to, Some(principal("avery")));
    }

    #[test]
    fn receiving_a_shared_file_does_not_make_you_a_member() {
        let mut fpl = desktop("avery", &[("avery", Role::Owner)]);
        fpl.shares.push(Share {
            path: "docs/q3.md".to_owned(),
            grantee: principal("dana"),
            role: Role::Reader,
            expires_at_ms: None,
        });

        // The object is readable...
        assert_eq!(
            fpl.shares[0].authorize(&principal("dana"), NOW),
            Ok(Role::Reader)
        );
        // ...and the desktop is still not hers, so the rest of the tree is not
        // reachable by having been sent one file.
        assert_eq!(
            fpl.relation(&principal("dana"), NOW),
            Err(Refusal::NotAMember)
        );
    }

    #[test]
    fn an_expired_share_is_refused_and_says_which_kind_of_no() {
        let share = Share {
            path: "docs/q3.md".to_owned(),
            grantee: principal("dana"),
            role: Role::Reader,
            expires_at_ms: Some(NOW - 1),
        };
        assert_eq!(
            share.authorize(&principal("dana"), NOW),
            Err(Refusal::Expired)
        );
        assert_eq!(
            share.authorize(&principal("someone-else"), NOW),
            Err(Refusal::NotGranted)
        );
    }

    #[test]
    fn a_lapsed_membership_reads_as_expired_not_absent() {
        let mut fpl = desktop("avery", &[]);
        fpl.members.push(Membership {
            principal: principal("kwame"),
            role: Role::Writer,
            expires_at_ms: Some(NOW - 1),
        });
        assert_eq!(
            fpl.relation(&principal("kwame"), NOW),
            Err(Refusal::Expired)
        );
        assert_eq!(
            fpl.relation(&principal("nobody"), NOW),
            Err(Refusal::NotAMember),
            "never admitted is a different answer from admitted and lapsed"
        );
    }

    #[test]
    fn the_strongest_live_membership_wins() {
        let mut fpl = desktop("avery", &[("kwame", Role::Reader), ("kwame", Role::Writer)]);
        // Same principal, two memberships: one lapsed reader, one live writer.
        fpl.members[0].expires_at_ms = Some(NOW - 1);
        assert_eq!(fpl.relation(&principal("kwame"), NOW), Ok(Role::Writer));
        assert!(fpl.relation(&principal("kwame"), NOW).unwrap() >= Role::Reader);
    }

    #[test]
    fn a_local_provider_is_not_a_shortcut_for_a_non_member() {
        let fpl = desktop("avery", &[("avery", Role::Owner)]);
        let studio = device("studio", "avery", None, &["soil:audio"]);
        let audio = Capability::from("soil:audio");
        let remote = Provider {
            device: "agora-one".to_owned(),
            capability: Capability::from("soil:audio"),
        };
        let host = BTreeSet::new();

        // A stranger gets the same answer whether the provider is under their
        // chair or across the tailnet, which is the whole property.
        assert_eq!(
            resolve_capability(
                &fpl,
                &principal("dana"),
                &studio,
                &audio,
                std::slice::from_ref(&remote),
                &host,
                NOW
            ),
            Resolution::Refused(Refusal::NotAMember)
        );
        let without_local = device("studio", "avery", None, &[]);
        assert_eq!(
            resolve_capability(
                &fpl,
                &principal("dana"),
                &without_local,
                &audio,
                &[remote],
                &host,
                NOW
            ),
            Resolution::Refused(Refusal::NotAMember)
        );
    }

    #[test]
    fn capabilities_resolve_narrowest_first() {
        let fpl = desktop("avery", &[("avery", Role::Owner)]);
        let audio = Capability::from("soil:audio");
        let studio = device("studio", "avery", None, &["soil:audio"]);
        let remote = Provider {
            device: "agora-one".to_owned(),
            capability: Capability::from("soil:audio"),
        };

        // The host implementing it beats everything, including a local provider.
        let host: BTreeSet<Capability> = [audio.clone()].into_iter().collect();
        assert_eq!(
            resolve_capability(
                &fpl,
                &principal("avery"),
                &studio,
                &audio,
                std::slice::from_ref(&remote),
                &host,
                NOW
            ),
            Resolution::InProcess
        );

        // A local provider beats a peer.
        let empty = BTreeSet::new();
        assert_eq!(
            resolve_capability(
                &fpl,
                &principal("avery"),
                &studio,
                &audio,
                std::slice::from_ref(&remote),
                &empty,
                NOW
            ),
            Resolution::Local {
                device: "studio".to_owned()
            }
        );

        // A peer is used when nobody here has it.
        let thin = device("phone", "avery", None, &[]);
        assert_eq!(
            resolve_capability(
                &fpl,
                &principal("avery"),
                &thin,
                &audio,
                &[remote],
                &empty,
                NOW
            ),
            Resolution::Peer {
                device: "agora-one".to_owned()
            }
        );

        // And nobody having it is a refusal with a reason, not a None.
        assert_eq!(
            resolve_capability(&fpl, &principal("avery"), &thin, &audio, &[], &empty, NOW),
            Resolution::Refused(Refusal::NoProvider)
        );
    }

    #[test]
    fn a_shared_desktop_can_require_a_spore() {
        let mut fpl = desktop("avery", &[("kwame", Role::Writer)]);
        fpl.installs.push(Install {
            spore: "dev.fungos.mail".to_owned(),
            version: Some("1.4".to_owned()),
            grants: BTreeMap::from([("soil:http".to_owned(), "mail.example".to_owned())]),
            required: true,
        });
        fpl.installs.push(Install {
            spore: "dev.fungos.sheets".to_owned(),
            version: None,
            grants: BTreeMap::new(),
            required: false,
        });

        let nothing: BTreeSet<String> = BTreeSet::new();
        let missing = fpl.unsatisfied(&nothing);
        assert_eq!(missing.len(), 1, "only the required one is compliance");
        assert_eq!(missing[0].spore, "dev.fungos.mail");

        // Optional installs are a preference, so a device without them is not
        // non-compliant.
        let mail_only: BTreeSet<String> = ["dev.fungos.mail".to_owned()].into_iter().collect();
        assert!(fpl.unsatisfied(&mail_only).is_empty());
    }

    #[test]
    fn the_owner_is_a_member_without_being_listed() {
        let fpl = desktop("avery", &[]);
        assert_eq!(fpl.relation(&principal("avery"), NOW), Ok(Role::Owner));
    }

    #[test]
    fn a_desktop_refuses_a_spore_even_where_it_is_installed() {
        let mut fpl = desktop("avery", &[("kwame", Role::Writer)]);
        fpl.denied.insert("dev.fungos.chatmail".to_owned());

        // Having it installed is not a way to run it.
        assert_eq!(fpl.permits("dev.fungos.chatmail"), Err(Refusal::Denied));
        // Everything not named is permitted: denying is a list, not a gate.
        assert_eq!(fpl.permits("dev.fungos.sheets"), Ok(()));
    }

    #[test]
    fn a_desktop_that_both_requires_and_refuses_a_spore_can_say_so() {
        let mut fpl = desktop("avery", &[("kwame", Role::Writer)]);
        fpl.denied.insert("dev.fungos.chatmail".to_owned());
        fpl.installs.push(Install {
            spore: "dev.fungos.chatmail".to_owned(),
            version: None,
            grants: BTreeMap::new(),
            required: true,
        });
        fpl.installs.push(Install {
            spore: "dev.fungos.sheets".to_owned(),
            version: None,
            grants: BTreeMap::new(),
            required: true,
        });

        // A configuration nobody could satisfy is named rather than left to
        // manifest as an install that never completes.
        assert_eq!(fpl.contradictions(), vec!["dev.fungos.chatmail"]);
        assert_eq!(fpl.permits("dev.fungos.chatmail"), Err(Refusal::Denied));
    }
}
