//! The night color pass over the frame (`Canvas::copy_xrgb`, the KMS copy, and `Canvas::rgba_out`, the snapshots):
//! it changes nothing at 6500 K and when off, it is the table on every pixel otherwise, and what it costs on the
//! 7-inch panel's 1024 x 600 frame (`cargo test --release -p display --test night_pass -- --ignored --nocapture`).

use common::config::DisplayConfig;
use common::night::Lut;
use display::canvas::Canvas;
use display::feed::{demo, Feed};
use display::headless::{render, Options};
use display::layout::Rect;
use std::time::Instant;

/// The demo screen at 1024 x 600: text, artwork, meters, every color the display uses.
fn frame() -> Canvas {
    let feed = Feed::from_json(&demo("both").unwrap(), std::path::Path::new(".")).unwrap();
    let o = Options {
        w: 1024,
        h: 600,
        cfg: DisplayConfig::default(),
        view: None,
        station: None,
        product: "Product".into(),
    };
    render(&feed, &o).0.canvas
}

fn xrgb(c: &Canvas, lut: &Lut) -> Vec<u8> {
    let pitch = 1024 * 4;
    let mut dst = vec![0u8; pitch * 600];
    c.copy_xrgb(Rect::new(0, 0, 1024, 600), &mut dst, pitch, lut);
    dst
}

#[test]
fn the_pass_leaves_every_pixel_unchanged_at_6500_k_and_off() {
    let c = frame();
    let plain = xrgb(&c, &Lut::identity());
    for lut in [Lut::identity(), Lut::for_look(6500.0, 0.0)] {
        assert!(lut.is_identity());
        assert_eq!(xrgb(&c, &lut), plain);
        assert_eq!(c.rgba_out(&lut), c.rgba());
    }
    // the copy is B, G, R, X of the canvas
    let px = &c.rgba()[..4];
    assert_eq!(&plain[..4], &[px[2], px[1], px[0], 0]);
}

#[test]
fn the_pass_is_the_table_on_every_pixel_and_the_same_for_the_panel_and_the_snapshot() {
    let c = frame();
    let lut = Lut::for_look(3400.0, 0.3);
    let out = xrgb(&c, &lut);
    let snap = c.rgba_out(&lut);
    for ((s, o), p) in c
        .rgba()
        .chunks_exact(4)
        .zip(out.chunks_exact(4))
        .zip(snap.chunks_exact(4))
    {
        let (r, g, b) = lut.map(s[0], s[1], s[2]);
        assert_eq!((o[2], o[1], o[0]), (r, g, b));
        assert_eq!((p[0], p[1], p[2], p[3]), (r, g, b, s[3]));
    }
    // a damaged rectangle only: the rest of the buffer is not written
    let pitch = 1024 * 4;
    let mut dst = vec![7u8; pitch * 600];
    c.copy_xrgb(Rect::new(10, 20, 30, 40), &mut dst, pitch, &lut);
    assert_eq!(dst[0], 7);
    assert_ne!(&dst[20 * pitch + 40..20 * pitch + 44], &[7, 7, 7, 7]);
}

#[test]
#[ignore]
fn cost_of_the_pass_on_a_1024x600_frame() {
    let c = frame();
    let n = 300;
    let mut dst = vec![0u8; 1024 * 4 * 600];
    let time = |lut: &Lut, dst: &mut Vec<u8>| {
        // warm up, then the mean of n full frames
        c.copy_xrgb(Rect::new(0, 0, 1024, 600), dst, 1024 * 4, lut);
        let t = Instant::now();
        for _ in 0..n {
            c.copy_xrgb(Rect::new(0, 0, 1024, 600), dst, 1024 * 4, lut);
        }
        t.elapsed().as_secs_f64() * 1000.0 / f64::from(n)
    };
    let plain = time(&Lut::identity(), &mut dst);
    let night = time(&Lut::for_look(3400.0, 0.3), &mut dst);
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(c.rgba_out(&Lut::for_look(3400.0, 0.0)));
    }
    let snap = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(Lut::for_look(1900.0 + f64::from(i), 0.1));
    }
    let build = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
    println!("1024 x 600, the whole frame copied to the output buffer (ordinary memory here):");
    println!("  identity (day, off: the table is skipped) {plain:.3} ms");
    println!(
        "  through the 3400 K table                 {night:.3} ms  (+{:.3} ms)",
        night - plain
    );
    println!("  snapshot copy through the table           {snap:.3} ms");
    println!("  building a table (768 entries)            {build:.3} ms");
}
