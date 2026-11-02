use soil_abi::CONTRACTS;
use wit_parser::Resolve;

#[test]
fn every_capability_contract_parses_with_its_reviewed_package_name() {
    for contract in CONTRACTS {
        let mut resolve = Resolve::default();
        let id = resolve
            .push_str(contract.file, contract.source)
            .unwrap_or_else(|error| {
                panic!("{} must be valid WIT: {error}", contract.file);
            });
        let package = &resolve.packages[id];
        assert_eq!(
            package.name.to_string(),
            contract.package,
            "{} changed its package name",
            contract.file
        );

        for (interface_name, functions) in contract.interfaces {
            let interface_id = *package.interfaces.get(*interface_name).unwrap_or_else(|| {
                panic!("{} is missing interface `{interface_name}`", contract.file)
            });
            let interface = &resolve.interfaces[interface_id];
            for function in *functions {
                assert!(
                    interface.functions.contains_key(*function),
                    "{}::{interface_name} is missing `{function}`",
                    contract.file
                );
            }
        }
    }
}

#[test]
fn capability_packages_are_versioned_and_namespaced() {
    for contract in CONTRACTS {
        assert!(
            contract.package.starts_with("soil:"),
            "{} must stay in the soil namespace",
            contract.file
        );
        assert!(
            contract.package.contains("@1."),
            "{} must carry a major version",
            contract.file
        );
    }
}

#[test]
fn package_names_are_unique() {
    for (index, contract) in CONTRACTS.iter().enumerate() {
        for other in &CONTRACTS[index + 1..] {
            assert_ne!(
                contract.package, other.package,
                "{} and {} expose the same package",
                contract.file, other.file
            );
        }
    }
}
