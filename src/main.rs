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

use std::path::PathBuf;

use asciime_filter::{charset, AsciiFilter, AsciiMap, GlyphMapBuilder, StreamProcessor};

use anyhow::Context;

use clap::Parser;

#[derive(Debug, Parser)]
#[clap(author, version, about)]
pub struct Opts {
    #[clap()]
    /// Path to the capture device
    source: String,
    #[clap()]
    /// Path to the output device
    sink: String,
    #[clap(short = 'c', long = "charset-bits", default_value_t = 6)]
    /// Number of bits to use for the charset
    nbits: u32,
    #[clap(short = 'f', long = "font")]
    /// Path to a font
    font: Option<PathBuf>,
    #[clap(short = 's', long = "size")]
    /// Font size (pixels)
    font_size: Option<f32>,
}

fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();

    let chars = charset(opts.nbits).context("No charset for that number of bits")?;
    println!("charset: {}", chars.iter().collect::<String>());
    let ascii_map = AsciiMap::new(chars);
    let mut glyphs = GlyphMapBuilder::new(&ascii_map);
    if let Some(font) = opts.font {
        glyphs = glyphs.with_font(font);
    }
    if let Some(size) = opts.font_size {
        glyphs = glyphs.with_scale(size);
    }
    let glyphs = glyphs.build()?;

    let ascii_filter = AsciiFilter::new(&ascii_map, &glyphs);
    let mut stream = StreamProcessor::new(&opts.source, &opts.sink)?.add_filter(&ascii_filter);
    loop {
        stream.process_frame()?;
    }
}
