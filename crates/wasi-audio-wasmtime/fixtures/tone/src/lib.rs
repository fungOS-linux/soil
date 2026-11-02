use soil_sdk::audio;

wit_bindgen::generate!({
    path: "wit",
    world: "runner",
    generate_all,
});

export!(Component);

struct Component;

impl Guest for Component {
    fn run() -> u32 {
        match audio::play_tone(48_000, 2, 440.0, 200, 0.2) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }
}
