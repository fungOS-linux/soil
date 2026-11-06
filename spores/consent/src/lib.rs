//! Manifest module for the native consent Spore.
//!
//! The real surface is the host-owned native popup in
//! `crates/soil-consent-popup`. This module exists so the Spore has a signed,
//! packageable identity even when launched natively.

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
});

struct ConsentSpore;

impl Guest for ConsentSpore {
    fn run() {}
}

export!(ConsentSpore);
