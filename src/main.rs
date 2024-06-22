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

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use anyhow::Context;
use clap::Parser;
use crossterm::{
    event::{self, Event as TEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{
    backend::CrosstermBackend,
    layout::Constraint,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, Row, Table},
    Terminal,
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
    #[clap(short = 'm', long = "mode", value_enum, default_value_t = Mode::Color)]
    /// Color mode
    mode: Mode,
    #[clap(short = 'I', long = "no-interactive")]
    /// Disable interactive mode
    nointeractive: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Mode {
    Grayscale,
    Color,
    Invert,
}

impl From<Mode> for AsciiMode {
    #[must_use]
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Grayscale => Self::Grayscale,
            Mode::Color => Self::Color,
            Mode::Invert => Self::Invert,
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

struct AppState<'cap, 'out> {
    source: String,
    sink: String,
    nbits: u32,
    chars: Vec<char>,
    ascii_filter: AsciiFilter<'static>,
    stream: StreamProcessor<'cap, 'out>,
    interactive: bool,
    enabled: bool,
    redraw: bool,
}

impl AppState<'_, '_> {
    fn from_opts(opts: Opts) -> anyhow::Result<Self> {
        let nbits = opts.nbits;
        let chars = charset(nbits).context("No charset for that number of bits")?;
        let glyphs = GlyphMapBuilder::new(&chars)
            .with_font_or_default(opts.font)
            .with_size_or_default(opts.font_size)
            .build()?;
        let ascii_map = AsciiMap::new(chars.clone());

        let ascii_filter = AsciiFilter::new(ascii_map, glyphs, opts.mode.into());
        let stream = StreamProcessor::new(&opts.source, &opts.sink)?
            .add_filter(Box::new(ascii_filter.clone()));

        Ok(Self {
            source: opts.source,
            sink: opts.sink,
            nbits,
            chars,
            ascii_filter,
            stream,
            interactive: !opts.nointeractive,
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
            self.stream.add_filter(Box::new(self.ascii_filter.clone()))
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
                .add_filter(Box::new(self.ascii_filter.clone()));
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
                .add_filter(Box::new(self.ascii_filter.clone()));
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
            self.ascii_filter = self.ascii_filter.set_charset(chars.clone());
            self.chars = chars;
            if self.enabled {
                self.stream = self
                    .stream
                    .clear_filters()
                    .add_filter(Box::new(self.ascii_filter.clone()));
            }
        }
        self
    }

    #[must_use]
    fn font_size(&self) -> u32 {
        let (w, h) = self.ascii_filter.size();
        debug_assert!(w == h, "{w} != {h}");
        w
    }

    #[must_use]
    const fn mode(&self) -> AsciiMode {
        self.ascii_filter.mode()
    }
}

fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    let mut app = AppState::from_opts(opts)?;

    let mut terminal = if app.interactive {
        enable_raw_mode().context("Failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        Some(Terminal::new(backend).context("Failed to create terminal")?)
    } else {
        None
    };

    let (tx, rx) = mpsc::channel();
    if app.interactive {
        thread::spawn(move || loop {
            if let Ok(TEvent::Key(key)) = event::read() {
                tx.send(key.into()).expect("Failed to send key event");
            }
        });
    }

    loop {
        app.stream.process_frame()?;
        if app.interactive {
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
            if app.redraw {
                terminal
                    .as_mut()
                    .unwrap()
                    .draw(|frame| {
                        let status = if app.enabled { "Enabled" } else { "Disabled" };
                        let mode = match app.mode() {
                            AsciiMode::Grayscale => "grayscale",
                            AsciiMode::Color => "color",
                            AsciiMode::Invert => "invert",
                        };
                        let font_size = app.font_size().to_string();
                        let nbits = app.nbits.to_string();
                        let chars = app.chars.iter().collect::<String>().replace(' ', "␣");

                        let size = frame.size();
                        let params = Table::new(vec![
                            Row::new(vec!["capture:", &app.source]),
                            Row::new(vec!["output:", &app.sink]),
                            Row::new(vec!["status (<SPACE>):", status]),
                            Row::new(vec!["mode (⏎):", mode]),
                            Row::new(vec!["size (+/-):", &font_size]),
                            Row::new(vec!["bit depth (⬅/➡):", &nbits]),
                            Row::new(vec!["charset:", &chars]),
                        ])
                        .block(Block::default().title(Span::styled(
                            "Parameters (Controls)",
                            Style::default().add_modifier(Modifier::BOLD),
                        )))
                        .widths(&[Constraint::Length(17), Constraint::Length(64)]);

                        frame.render_widget(params, size);
                    })
                    .context("Failed to write to terminal")?;
                app.redraw = false;
            }
        }
    }

    if app.interactive {
        disable_raw_mode().context("Failed to disable raw mode")?;
        execute!(
            terminal.as_mut().unwrap().backend_mut(),
            LeaveAlternateScreen
        )?;
    }

    Ok(())
}
