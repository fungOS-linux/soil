//! Canonical Soil host capability contracts.
//!
//! Each `soil:*` package is a WIT contract under `wit/` plus a conformance
//! test. This crate is the single place that embeds the contract sources, so a
//! package rename, interface rename or dropped function fails `cargo test`
//! rather than silently changing the ABI.

/// A canonical capability package and the interface functions it must expose.
#[derive(Debug)]
pub struct Contract {
    /// Repository-relative path, used in assertions and failure messages.
    pub file: &'static str,
    /// Exact WIT source; embedded so tests do not depend on the cwd.
    pub source: &'static str,
    /// Fully qualified package name, e.g. `soil:time@1.0.0`.
    pub package: &'static str,
    /// Interface name plus the free functions that must exist on it.
    pub interfaces: &'static [(&'static str, &'static [&'static str])],
}

/// Every package currently exposed to a Spore.
pub const CONTRACTS: &[Contract] = &[
    Contract {
        file: "wit/soil-view.wit",
        source: include_str!("../../../wit/soil-view.wit"),
        package: "soil:view@1.0.0",
        interfaces: &[("viewer", &["open"])],
    },
    Contract {
        file: "wit/soil-time.wit",
        source: include_str!("../../../wit/soil-time.wit"),
        package: "soil:time@1.0.0",
        interfaces: &[("time", &["monotonic-ms", "tick"])],
    },
    Contract {
        file: "wit/soil-storage.wit",
        source: include_str!("../../../wit/soil-storage.wit"),
        package: "soil:storage@1.0.0",
        interfaces: &[("storage", &["read", "write"])],
    },
    Contract {
        file: "wit/soil-canvas.wit",
        source: include_str!("../../../wit/soil-canvas.wit"),
        package: "soil:canvas@1.0.0",
        interfaces: &[("surface", &["present", "observe", "input", "close"])],
    },
    Contract {
        file: "wit/soil-ble.wit",
        source: include_str!("../../../wit/soil-ble.wit"),
        package: "soil:ble@1.0.0",
        interfaces: &[(
            "ble",
            &[
                "scan",
                "connect",
                "disconnect",
                "services",
                "read",
                "write",
                "subscribe",
                "next-event",
            ],
        )],
    },
];
