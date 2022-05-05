#![warn(deprecated_in_future)]
#![warn(future_incompatible)]
#![warn(nonstandard_style)]
#![warn(rust_2018_compatibility)]
#![warn(rust_2018_idioms)]
#![warn(trivial_casts, trivial_numeric_casts)]
#![warn(unused)]
#![warn(clippy::all, clippy::pedantic)]
#![warn(clippy::missing_const_for_fn)]
#![warn(clippy::use_self)]
#![warn(clippy::if_then_some_else_none)]

use asciime_filter::{charset, AsciiFilter, AsciiMap, GlyphMapBuilder, StreamProcessor};

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let chars = charset(6).context("No charset for that number of bits")?;
    println!("charset: {}", chars.iter().collect::<String>());
    let ascii_map = AsciiMap::new(chars);
    let glyphs = GlyphMapBuilder::new(&ascii_map).build()?;

    // TODO: Make these arguments
    let source = "/dev/video0";
    let sink = "/dev/video4";

    let ascii_filter = AsciiFilter::new(&ascii_map, &glyphs);
    let mut stream = StreamProcessor::new(source, sink)?.add_filter(&ascii_filter);
    loop {
        stream.process_frame()?;
    }
}
