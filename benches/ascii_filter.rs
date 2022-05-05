use asciime_filter::{charset, AsciiFilter, AsciiMap, Frame, FrameFilter, GlyphMapBuilder};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

pub fn ascii_filter_bench(c: &mut Criterion) {
    let ascii_map = AsciiMap::new(charset(6).unwrap());
    let glyphs = GlyphMapBuilder::new(&ascii_map).build().unwrap();
    let ascii_filter = AsciiFilter::new(&ascii_map, &glyphs);

    let width: u32 = 1280;
    let height: u32 = 720;
    let size = (width * height * 2) as usize;

    let empty_buf = vec![0; size];

    let seed: usize = 123;
    let arbitrary_buf = (0..size)
        .map(|i| ((seed + i) * (i + 1)) as u8)
        .collect::<Vec<_>>();

    let mut group = c.benchmark_group("AsciiFilter");
    group.bench_function("empty frame", |b| {
        b.iter_batched(
            || empty_buf.clone(),
            |mut buf| {
                let mut frame = Frame::new(&mut buf, width, height);
                ascii_filter.process(&mut frame);
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("arbitrary frame", |b| {
        b.iter_batched(
            || arbitrary_buf.clone(),
            |mut buf| {
                let mut frame = Frame::new(&mut buf, width, height);
                ascii_filter.process(&mut frame);
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(ascii_filter_benches, ascii_filter_bench);
criterion_main!(ascii_filter_benches);
