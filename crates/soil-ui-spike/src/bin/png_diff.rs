// Compare two renders: how many pixels differ, and by how much.
//
// Byte equality is the wrong bar for a GPU renderer that may order its work
// differently between runs; what matters is whether the picture changed.
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (a, b) = match (args.next(), args.next()) {
        (Some(a), Some(b)) => (a, b),
        _ => anyhow::bail!("usage: png_diff <a.png> <b.png>"),
    };
    let a = image::open(&a)?.to_rgba8();
    let b = image::open(&b)?.to_rgba8();
    if a.dimensions() != b.dimensions() {
        anyhow::bail!("size differs: {:?} vs {:?}", a.dimensions(), b.dimensions());
    }
    let mut differing = 0u64;
    let mut max_delta = 0u8;
    let mut worst = (0u32, 0u32);
    for (x, y, pa) in a.enumerate_pixels() {
        let pb = b.get_pixel(x, y);
        let delta = pa.0.iter().zip(pb.0.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        if delta > 0 {
            differing += 1;
            if delta > max_delta {
                max_delta = delta;
                worst = (x, y);
            }
        }
    }
    let total = u64::from(a.width()) * u64::from(a.height());
    println!("{differing} of {total} pixels differ, largest channel delta {max_delta} at {worst:?}");
    Ok(())
}
