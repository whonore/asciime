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
use std::sync::mpsc;
use std::thread;

use anyhow::Context;
use clap::Parser;
use crossterm::{
    event::{self, Event as TEvent, KeyCode, KeyEvent, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};

use asciime_filter::{charset, AsciiFilter, AsciiMap, AsciiMode, GlyphMapBuilder, StreamProcessor};

const SIZE_INCREMENT: i32 = 1;
const BIG_SIZE_INCREMENT: i32 = 10;

#[derive(Debug, Parser)]
#[clap(author, version, about)]
pub struct Opts {
    #[clap()]
    /// Path to the capture device
    source: String,
    #[clap()]
    /// Path to the output device
    sink: String,
    #[clap(short = 'b', long = "bitdepth", default_value_t = 6)]
    /// Number of bits to use for the charset
    nbits: u32,
    #[clap(short = 'f', long = "font")]
    /// Path to a font
    font: Option<PathBuf>,
    #[clap(short = 's', long = "size")]
    /// Font size (pixels)
    font_size: Option<u32>,
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
    #[must_use]
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Grayscale => Self::Grayscale,
            Mode::Color => Self::Color,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum MoreLess {
    More,
    Less,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
enum Event {
    Quit,
    Toggle,
    CycleMode,
    ChangeSize(i32),
    ChangeBitdepth(MoreLess),
    Other,
}

impl From<KeyEvent> for Event {
    #[must_use]
    fn from(key: KeyEvent) -> Self {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Self::Quit,
            KeyCode::Char(' ') => Self::Toggle,
            KeyCode::Enter => Self::CycleMode,
            KeyCode::Char(c @ ('+' | '-')) => {
                let sign = if c == '+' { 1 } else { -1 };
                let inc = if key.modifiers.contains(KeyModifiers::ALT) {
                    BIG_SIZE_INCREMENT
                } else {
                    SIZE_INCREMENT
                };
                Self::ChangeSize(sign * inc)
            }
            KeyCode::Left => Self::ChangeBitdepth(MoreLess::Less),
            KeyCode::Right => Self::ChangeBitdepth(MoreLess::More),
            _ => Self::Other,
        }
    }
}

struct AppState<'ascii, 'cap, 'out> {
    nbits: u32,
    ascii_filter: AsciiFilter<'ascii>,
    stream: StreamProcessor<'cap, 'out, AsciiFilter<'ascii>>,
    enabled: bool,
    redraw: bool,
}

impl AppState<'_, '_, '_> {
    fn from_opts(opts: Opts) -> anyhow::Result<Self> {
        let nbits = opts.nbits;
        let chars = charset(nbits).context("No charset for that number of bits")?;
        println!("charset: {}", chars.iter().collect::<String>());
        let glyphs = GlyphMapBuilder::new(&chars)
            .with_font_or_default(opts.font)
            .with_size_or_default(opts.font_size)
            .build()?;
        let ascii_map = AsciiMap::new(chars);

        let ascii_filter = AsciiFilter::new(ascii_map, glyphs, opts.mode.into());
        let stream =
            StreamProcessor::new(&opts.source, &opts.sink)?.add_filter(ascii_filter.clone());

        Ok(Self {
            nbits,
            ascii_filter,
            stream,
            enabled: true,
            redraw: true,
        })
    }

    #[must_use]
    fn toggle(mut self) -> Self {
        self.redraw = true;
        self.stream = if self.enabled {
            self.stream.clear_filters()
        } else {
            self.stream.add_filter(self.ascii_filter.clone())
        };
        self.enabled = !self.enabled;
        self
    }

    #[must_use]
    fn cycle_mode(mut self) -> Self {
        self.redraw = true;
        self.ascii_filter = self.ascii_filter.cycle_mode();
        if self.enabled {
            self.stream = self
                .stream
                .clear_filters()
                .add_filter(self.ascii_filter.clone());
        }
        self
    }

    #[must_use]
    fn change_size(mut self, inc: i32) -> Self {
        self.redraw = true;
        self.ascii_filter = self.ascii_filter.resize(inc);
        if self.enabled {
            self.stream = self
                .stream
                .clear_filters()
                .add_filter(self.ascii_filter.clone());
        }
        self
    }

    #[must_use]
    fn change_bitdepth(mut self, moreless: MoreLess) -> Self {
        let new_nbits = match moreless {
            MoreLess::More => self.nbits + 1,
            MoreLess::Less => self.nbits - 1,
        };
        if let Some(chars) = charset(new_nbits) {
            self.redraw = true;
            self.nbits = new_nbits;
            self.ascii_filter = self.ascii_filter.set_charset(chars);
            if self.enabled {
                self.stream = self
                    .stream
                    .clear_filters()
                    .add_filter(self.ascii_filter.clone());
            }
        }
        self
    }
}

fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    let mut app = AppState::from_opts(opts)?;

    enable_raw_mode().context("Failed to enable raw mode")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        if let Ok(TEvent::Key(key)) = event::read() {
            tx.send(key.into()).expect("Failed to send key event");
        }
    });

    loop {
        app.stream.process_frame()?;
        if let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Quit => {
                    break;
                }
                Event::Toggle => {
                    app = app.toggle();
                }
                Event::CycleMode => {
                    app = app.cycle_mode();
                }
                Event::ChangeSize(inc) => {
                    app = app.change_size(inc);
                }
                Event::ChangeBitdepth(moreless) => {
                    app = app.change_bitdepth(moreless);
                }
                _ => {}
            }
        }
    }

    disable_raw_mode().context("Failed to disable raw mode")?;
    Ok(())
}
