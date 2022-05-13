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

use anyhow::Context;
use clap::Parser;

use asciime_filter::{charset, AsciiFilter, AsciiMap, AsciiMode, GlyphMapBuilder, StreamProcessor};

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
    #[clap(short = 'm', long = "mode", arg_enum, default_value_t = Mode::Color)]
    /// Color mode
    mode: Mode,
}

#[derive(Debug, Clone, Copy, clap::ArgEnum)]
enum Mode {
    Grayscale,
    Color,
}

impl From<Mode> for AsciiMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Grayscale => Self::Grayscale,
            Mode::Color => Self::Color,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();

    let chars = charset(opts.nbits).context("No charset for that number of bits")?;
    println!("charset: {}", chars.iter().collect::<String>());
    let ascii_map = AsciiMap::new(chars);
    let glyphs = GlyphMapBuilder::new(&ascii_map)
        .with_font_or_default(opts.font)
        .with_size_or_default(opts.font_size)
        .build()?;

    let ascii_filter = AsciiFilter::new(&ascii_map, &glyphs, opts.mode.into());
    let mut stream = StreamProcessor::new(&opts.source, &opts.sink)?.add_filter(&ascii_filter);
    loop {
        stream.process_frame()?;
    }
}
