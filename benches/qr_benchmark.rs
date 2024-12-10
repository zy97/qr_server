use std::io::Cursor;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use image::{ImageFormat, Luma};
use qrcode::QrCode;

fn fibonacci(n: u64) -> u64 {
    match n {
        0 => 1,
        1 => 1,
        n => fibonacci(n - 1) + fibonacci(n - 2),
    }
}
fn qr() -> Cursor<Vec<u8>> {
    // Encode some data into bits.
    let code = QrCode::new(b"zy").unwrap();

    // Render the bits into an image.
    let image = code.render::<Luma<u8>>().build();

    // Save the image.

    // image.save("/tmp/qrcode.png").unwrap();
    let mut buffer = Cursor::new(Vec::new());
    image.write_to(&mut buffer, ImageFormat::Png).unwrap();
    buffer
}

fn criterion_benchmark(c: &mut Criterion) {
    // c.bench_function("fib 20", |b| b.iter(|| fibonacci(black_box(20))));
    c.bench_function("qr test", |b| b.iter(|| qr()));
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
