use std::{env, fs};
fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: soil-run MODULE.wasm [fuel]");
        std::process::exit(2)
    });
    let fuel = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(soil_runtime::DEFAULT_FUEL);
    let result = fs::read(&path)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| soil_runtime::run(&bytes, fuel));
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("soil-run: {error:#}");
            std::process::exit(1);
        }
    }
}
