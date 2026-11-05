wit_bindgen::generate!({ path: "wit", world: "fixture", generate_all });

use fungos::actions::catalog;

struct Component;

export!(Component);

impl Guest for Component {
    fn run() -> Result<u32, String> {
        let actions = catalog::list_actions().map_err(|error| format!("{error:?}"))?;
        Ok(actions.len() as u32)
    }
}
