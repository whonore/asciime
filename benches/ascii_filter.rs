use asciime_filter::{
    AsciiFilter, AsciiMap, Frame, FrameFilter, GlyphMap, ASCII_MAP_64, FONT, FONT_SCALE,
};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

pub fn ascii_filter_bench(c: &mut Criterion) {
    let ascii_map = AsciiMap::new(&ASCII_MAP_64);
    let glyphs = GlyphMap::new(FONT, FONT_SCALE, &ASCII_MAP_64).unwrap();
    let ascii_filter = AsciiFilter::new(&ascii_map, &glyphs);

    let width: u32 = 1280;
    let height: u32 = 720;
    let size = (width * height * 2) as usize;

    let buf = vec![0; size];
    let empty_frame = Frame::new(buf, width, height);

    let seed: usize = 123;
    let buf = (0..size).map(|i| ((seed + i) * (i + 1)) as u8).collect();
    let arbitrary_frame = Frame::new(buf, width, height);

    let mut group = c.benchmark_group("AsciiFilter");
    group.bench_function("empty frame", |b| {
        b.iter_batched(
            || empty_frame.clone(),
            |frame| ascii_filter.process(frame),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("arbitrary frame", |b| {
        b.iter_batched(
            || arbitrary_frame.clone(),
            |frame| ascii_filter.process(frame),
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(ascii_filter_benches, ascii_filter_bench);
criterion_main!(ascii_filter_benches);
