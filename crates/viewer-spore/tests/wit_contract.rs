use wit_parser::Resolve;

#[test]
fn soil_view_contract_parses_and_keeps_the_reviewed_package() {
    let mut resolve = Resolve::default();
    let id = resolve
        .push_str(
            "wit/soil-view.wit",
            include_str!("../../../wit/soil-view.wit"),
        )
        .expect("soil:view must be valid WIT");
    let package = &resolve.packages[id];
    assert_eq!(package.name.to_string(), "soil:view@1.0.0");
    assert!(package.interfaces.contains_key("viewer"));
}
