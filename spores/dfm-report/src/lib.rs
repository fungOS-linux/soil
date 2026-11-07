//! DFM report Spore.
//!
//! The module is deliberately thin: it accepts the launch args and reports the
//! model it was handed. Everything the user sees is either the Lua projection in
//! `ui/report.lua` or the native `soil-dfm-viz` window, which reads the same
//! `model/data.json`. Nothing here re-derives a DFM result.

wit_bindgen::generate!({ path: "wit", world: "dfm", generate_all });

struct Component;

export!(Component);

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<(), String> {
        let scene = args
            .iter()
            .position(|a| a == "--scene")
            .and_then(|i| args.get(i + 1));
        match scene {
            Some(path) => {
                println!("dfm-report: scene {path}");
                Ok(())
            }
            None => Err("dfm-report: pass --scene <data.json>".into()),
        }
    }
}
