//! Writes the demo image: `cargo run --example render_demo -- out.png [YYYY-MM-DD]`
fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "display.png".into());
    let today = args
        .next()
        .map(|d| d.parse().expect("date YYYY-MM-DD"))
        .unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(2026, 12, 14).unwrap());
    let img = running_viz::render::render(&running_viz::render::demo_stats(today));
    std::fs::write(&out, img.to_png(2)).unwrap();
    println!("wrote {out}");
}
