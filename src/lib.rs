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
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)] // TODO: remove

// TODO: document everything

use std::cmp;
use std::collections::HashMap;
use std::fs;
use std::ops::Index;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use itertools::Itertools;
use rayon::prelude::*;
use rusttype::{point, Font, Scale, ScaledGlyph};
use v4l::{
    buffer::Type,
    format::fourcc::FourCC,
    io::traits::{CaptureStream, OutputStream},
    prelude::*,
    video::{output::Parameters as OutputParameters, Capture, Output},
};

// $@B%8&WM#*oahkbdpqwmZO0QLCJUYXzcvunxrjft/\|()1{}[]?-_+~<>i!lI;:,"^`'.
const ASCII_MAP_NBITS: u32 = 6;
const ASCII_MAP_64: [char; 2_usize.pow(ASCII_MAP_NBITS)] = [
    '$', '@', 'B', '%', '&', 'W', 'M', '#', '*', 'o', 'h', 'k', 'b', 'd', 'p', 'q', 'w', 'm', 'Z',
    '0', 'Q', 'L', 'C', 'J', 'U', 'Y', 'X', 'z', 'c', 'u', 'n', 'x', 'r', 'j', 'f', '/', '\\', '|',
    '(', ')', '1', '{', '}', '[', ']', '?', '-', '_', '+', '~', '<', '>', 'i', '!', 'I', ';', ':',
    ',', '"', '^', '`', '\'', '.', ' ',
];

const DEFAULT_FONT: &[u8] = include_bytes!("../font/FiraCode-VF.ttf");
const DEFAULT_FONT_SCALE: u32 = 10;

// TODO: Set this based on frame size
const NSUBFRAME_SPLITS: u32 = 4;

#[must_use]
pub fn charset(nbits: u32) -> Option<Vec<char>> {
    (1..=ASCII_MAP_NBITS).contains(&nbits).then(|| {
        let step = 2_usize.pow(ASCII_MAP_NBITS - nbits);
        let mut chars = ASCII_MAP_64
            .iter()
            .step_by(step)
            .copied()
            .collect::<Vec<_>>();
        *chars.last_mut().unwrap() = ' ';
        chars
    })
}

#[derive(Debug, Clone)]
pub struct RenderedGlyph(Vec<(u32, u32, Brightness)>);

impl RenderedGlyph {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[must_use]
    pub fn new(glyph: ScaledGlyph<'_>) -> Self {
        let scale = glyph.scale().x as u32;
        let glyph = glyph.positioned(point(0.0, 0.0));
        let bb = glyph.pixel_bounding_box().unwrap_or_default();
        let width = bb.width() as u32;
        let height = bb.height() as u32;

        let mut pts = vec![];
        glyph.draw(|x, y, v| {
            pts.push((x, y, Brightness::from(v)));
        });
        // Fill in missing columns
        for x in width..scale {
            for y in 0..height {
                pts.push((x, y, Brightness::default()));
            }
        }
        // Fill in missing rows
        for y in height..scale {
            for x in 0..scale {
                pts.push((x, y, Brightness::default()));
            }
        }

        debug_assert!(pts.iter().map(|(x, y, _)| (x, y)).all_unique(), "{pts:?}");
        debug_assert!(
            pts.len() == (scale * scale) as usize,
            "pts.len()={} scale={}",
            pts.len(),
            scale
        );

        Self(pts)
    }
}

pub struct GlyphMapBuilder<'chars> {
    font: Option<PathBuf>,
    size: Option<u32>,
    chars: &'chars [char],
}

impl<'chars> GlyphMapBuilder<'chars> {
    #[must_use]
    pub const fn new(chars: &'chars [char]) -> Self {
        Self {
            font: None,
            size: None,
            chars,
        }
    }

    #[must_use]
    pub fn with_font<P>(mut self, font: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.font = Some(font.as_ref().into());
        self
    }

    #[must_use]
    pub fn with_font_or_default<P>(self, font: Option<P>) -> Self
    where
        P: AsRef<Path>,
    {
        if let Some(font) = font {
            self.with_font(font)
        } else {
            self
        }
    }

    #[must_use]
    pub const fn with_size(mut self, size: u32) -> Self {
        self.size = Some(size);
        self
    }

    #[must_use]
    pub const fn with_size_or_default(self, size: Option<u32>) -> Self {
        if let Some(size) = size {
            self.with_size(size)
        } else {
            self
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn build(self) -> anyhow::Result<GlyphMap<'static>> {
        let data = self.font.map_or(Ok(DEFAULT_FONT.to_vec()), |path| {
            fs::read(&path).with_context(|| format!("Failed to read font file {}", path.display()))
        })?;
        let font = Font::try_from_vec(data).context("Failed to load font")?;
        Ok(GlyphMap::new(
            font,
            Scale::uniform(self.size.unwrap_or(DEFAULT_FONT_SCALE) as f32),
            self.chars,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct GlyphMap<'font> {
    font: Font<'font>,
    glyphs: HashMap<char, RenderedGlyph>,
    width: u32,
    height: u32,
}

impl<'font> GlyphMap<'font> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[must_use]
    pub fn new(font: Font<'font>, scale: Scale, chars: &[char]) -> Self {
        let glyphs = chars
            .iter()
            .map(|&c| (c, RenderedGlyph::new(font.glyph(c).scaled(scale))))
            .collect();
        Self {
            font,
            glyphs,
            width: scale.x as u32,
            height: scale.y as u32,
        }
    }

    #[must_use]
    pub fn get(&self, c: &char) -> Option<&RenderedGlyph> {
        self.glyphs.get(c)
    }

    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    #[must_use]
    pub fn resize(mut self, inc: i32) -> Self {
        let add_signed = |x: u32, y: i32| -> u32 { cmp::max((x as i32) + y, 1) as u32 };
        self.width = add_signed(self.width, inc);
        self.height = add_signed(self.height, inc);
        let scale = Scale {
            x: self.width as f32,
            y: self.height as f32,
        };
        for (c, g) in &mut self.glyphs {
            *g = RenderedGlyph::new(self.font.glyph(*c).scaled(scale));
        }
        self
    }

    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn set_charset(self, chars: &[char]) -> Self {
        let scale = Scale {
            x: self.width as f32,
            y: self.height as f32,
        };
        Self::new(self.font, scale, chars)
    }
}

#[derive(Debug, Clone)]
pub struct AsciiMap {
    map: Vec<char>,
    nbits: u32,
}

impl AsciiMap {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    #[must_use]
    pub fn new(map: Vec<char>) -> Self {
        let nbits = (map.len() as f32).log2() as u32;
        debug_assert!(
            2usize.pow(nbits) == map.len(),
            "len={}, nbits={}",
            map.len(),
            nbits
        );
        debug_assert!(nbits <= u8::BITS, "nbits={nbits}");
        Self { map, nbits }
    }

    #[must_use]
    pub fn chars(&self) -> &[char] {
        &self.map
    }

    pub fn invert(&mut self) {
        self.map.reverse();
    }
}

impl Index<Brightness> for AsciiMap {
    type Output = char;

    #[must_use]
    fn index(&self, idx: Brightness) -> &Self::Output {
        let idx = idx.0 >> (u8::BITS - self.nbits);
        &self.map[idx as usize]
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Brightness(u8);

impl Brightness {
    #[must_use]
    pub fn as_ascii(self, map: &AsciiMap) -> char {
        map[self]
    }
}

impl From<f32> for Brightness {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[must_use]
    fn from(b: f32) -> Self {
        Self((b * 255.0).clamp(0.0, 255.0) as u8)
    }
}

#[derive(Debug)]
struct Yuyv<'pix> {
    buf: &'pix mut [u8],
    width: u32,
    height: u32,
}

impl<'pix> Yuyv<'pix> {
    #[must_use]
    pub fn new(buf: &'pix mut [u8], width: u32, height: u32) -> Self {
        Self { buf, width, height }
    }

    #[must_use]
    pub fn splitn(&mut self, n: u32) -> Vec<Yuyv<'_>> {
        debug_assert!(n > 0);
        debug_assert!(self.height % (2_u32.pow(n)) == 0);
        let len = self.buf.len() >> n;
        let height = self.height >> n;
        self.buf
            .chunks_exact_mut(len)
            .map(|sub| Yuyv::new(sub, self.width, height))
            .collect()
    }

    #[must_use]
    pub const fn iter_avg(&self, width: u32, height: u32) -> IterAvg<'_> {
        IterAvg::new(self, width, height)
    }

    #[must_use]
    pub const fn get_brightness(&self, x: u32, y: u32) -> Brightness {
        Brightness(self.buf[self.xy_to_idx(x, y)])
    }

    pub fn set_brightness<B>(&mut self, x: u32, y: u32, b: B)
    where
        B: Into<Brightness>,
    {
        let idx = self.xy_to_idx(x, y);
        self.buf[idx] = b.into().0;
    }

    pub fn as_grayscale(&mut self) {
        self.buf.fill(127);
    }

    // https://egeeks.github.io/kernal/media/V4L2-PIX-FMT-YUYV.html
    // 0       1       2       3
    // Y1 U1/2 Y2 V1/2 Y3 U3/4 Y4 V3/4      0
    // Y5 U5/6 Y6 V5/6 Y7 U7/8 Y8 V7/8      1
    #[must_use]
    const fn xy_to_idx(&self, x: u32, y: u32) -> usize {
        (2 * (y * self.width + x)) as usize
    }
}

#[derive(Debug)]
struct IterAvg<'pix> {
    pixels: &'pix Yuyv<'pix>,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    done: bool,
}

impl<'pix> IterAvg<'pix> {
    #[must_use]
    const fn new(pixels: &'pix Yuyv<'pix>, width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
            x: 0,
            y: 0,
            done: false,
        }
    }
}

impl Iterator for IterAvg<'_> {
    type Item = (u32, u32, Brightness);

    #[allow(clippy::cast_possible_truncation)]
    #[must_use]
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            // Average the brightness of the next group_sz pixels in the x and
            // y directions.
            let next_x = cmp::min(self.x + self.width, self.pixels.width);
            let next_y = cmp::min(self.y + self.height, self.pixels.height);
            let npix = (next_x - self.x) * (next_y - self.y);
            let avg = (self.x..next_x)
                .cartesian_product(self.y..next_y)
                .map(|(x, y)| u32::from(self.pixels.get_brightness(x, y).0))
                .sum::<u32>()
                / npix;
            debug_assert!(avg <= u8::MAX.into(), "avg={avg}");
            let ret = (self.x, self.y, Brightness(avg as u8));

            if next_x < self.pixels.width {
                self.x = next_x;
            } else if next_y < self.pixels.height {
                self.x = 0;
                self.y = next_y;
            } else {
                self.done = true;
            }
            Some(ret)
        }
    }
}

#[derive(Debug)]
pub struct Frame<'pix> {
    pixels: Yuyv<'pix>,
}

impl<'pix> Frame<'pix> {
    #[must_use]
    pub fn new(buf: &'pix mut [u8], width: u32, height: u32) -> Self {
        Self {
            pixels: Yuyv::new(buf, width, height),
        }
    }

    #[must_use]
    pub fn splitn(&mut self, n: u32) -> Vec<Frame<'_>> {
        self.pixels
            .splitn(n)
            .into_iter()
            .map(|pixels| Frame { pixels })
            .collect()
    }

    pub fn as_grayscale(&mut self) {
        self.pixels.as_grayscale();
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.pixels.buf
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.pixels.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.pixels.height
    }
}

pub trait FrameFilter {
    fn process(&self, frame: &mut Frame<'_>);
}

#[derive(Debug, Clone, Copy)]
pub enum AsciiMode {
    Grayscale,
    Color,
    Invert,
}

impl AsciiMode {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Grayscale => Self::Color,
            Self::Color => Self::Invert,
            Self::Invert => Self::Grayscale,
        }
    }
}

#[derive(Clone)]
pub struct AsciiFilter<'font> {
    ascii_map: AsciiMap,
    glyphs: GlyphMap<'font>,
    mode: AsciiMode,
}

impl<'font> AsciiFilter<'font> {
    #[must_use]
    pub fn new(mut ascii_map: AsciiMap, glyphs: GlyphMap<'font>, mode: AsciiMode) -> Self {
        if matches!(mode, AsciiMode::Invert) {
            ascii_map.invert();
        }
        Self {
            ascii_map,
            glyphs,
            mode,
        }
    }

    #[must_use]
    pub fn cycle_mode(mut self) -> Self {
        let old_mode = self.mode;
        self.mode = self.mode.next();
        if matches!(old_mode, AsciiMode::Invert) || matches!(self.mode, AsciiMode::Invert) {
            self.ascii_map.invert();
        }
        self
    }

    #[must_use]
    pub const fn mode(&self) -> AsciiMode {
        self.mode
    }

    #[must_use]
    pub fn resize(mut self, inc: i32) -> Self {
        self.glyphs = self.glyphs.resize(inc);
        self
    }

    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.glyphs.width, self.glyphs.height)
    }

    #[must_use]
    pub fn set_charset(mut self, chars: Vec<char>) -> Self {
        self.glyphs = self.glyphs.set_charset(&chars);
        self.ascii_map = AsciiMap::new(chars);
        self
    }
}

impl FrameFilter for AsciiFilter<'_> {
    fn process(&self, frame: &mut Frame<'_>) {
        let mut buf = frame.as_bytes().to_vec();
        let mut old_frame = Frame::new(&mut buf, frame.width(), frame.height());
        match self.mode {
            AsciiMode::Grayscale => frame.as_grayscale(),
            AsciiMode::Color | AsciiMode::Invert => {}
        }

        let mut subframes = frame.splitn(NSUBFRAME_SPLITS);
        let old_subframes = old_frame.splitn(NSUBFRAME_SPLITS);
        subframes
            .par_iter_mut()
            .zip(old_subframes)
            .for_each(|(subframe, old_subframe)| {
                let width = old_subframe.width();
                let height = old_subframe.height();
                old_subframe
                    .pixels
                    .iter_avg(self.glyphs.width, self.glyphs.height)
                    .flat_map(|(x, y, pix)| {
                        self.glyphs
                            .get(&pix.as_ascii(&self.ascii_map))
                            .unwrap()
                            .0
                            .iter()
                            .filter_map(move |(xoff, yoff, b)| {
                                let x = x + xoff;
                                let y = y + yoff;
                                (x < width && y < height).then_some((x, y, b))
                            })
                    })
                    .for_each(|(x, y, b)| subframe.pixels.set_brightness(x, y, *b));
            });
    }
}

pub struct StreamProcessor<'cap, 'out> {
    cap_stream: MmapStream<'cap>,
    out_stream: MmapStream<'out>,
    filters: Vec<Box<dyn FrameFilter>>,
    width: u32,
    height: u32,
}

impl StreamProcessor<'_, '_> {
    pub fn new(source: &str, sink: &str) -> anyhow::Result<Self> {
        // Prepare capture and output devices
        let cap = Device::with_path(source).context("Failed to open capture device")?;
        let out = Device::with_path(sink).context("Failed to open output device")?;

        // Confirm capture and output settings match and are valid
        let mut cap_fmt = Capture::format(&cap).context("Failed to read capture format")?;
        cap_fmt.fourcc = FourCC::new(b"YUYV");
        let cap_fmt =
            Capture::set_format(&cap, &cap_fmt).context("Failed to set capture format")?;
        let out_fmt = Output::set_format(&out, &cap_fmt).context("Failed to set output format")?;
        let cap_params = Capture::params(&cap).context("Failed to read capture parameters")?;
        let out_params = Output::set_params(&out, &OutputParameters::new(cap_params.interval))
            .context("Failed to set output parameters")?;

        if cap_fmt.fourcc.str()? != "YUYV" {
            return Err(anyhow!(
                "Unsupported fourcc: {}",
                cap_fmt.fourcc.str().unwrap()
            ));
        }

        if cap_fmt.width != out_fmt.width
            || cap_fmt.height != out_fmt.height
            || cap_fmt.fourcc != out_fmt.fourcc
            || cap_params.interval.numerator != out_params.interval.numerator
            || cap_params.interval.denominator != out_params.interval.denominator
        {
            return Err(anyhow!(
                "Output parameters do not match capture:\n\
                 Capture device:\n{}{}\nOutput device:\n{}{}",
                cap_fmt,
                cap_params,
                out_fmt,
                out_params,
            ));
        }

        println!(
            "Capture device:\n{}{}{}\nOutput device:\n{}{}{}",
            cap.query_caps()
                .context("Failed to read capture capabilities")?,
            cap_fmt,
            cap_params,
            out.query_caps()
                .context("Failed to read output capabilities")?,
            out_fmt,
            out_params,
        );

        // Prepare capture and output streams
        let cap_stream =
            MmapStream::new(&cap, Type::VideoCapture).context("Failed to open capture stream")?;
        let out_stream =
            MmapStream::new(&out, Type::VideoOutput).context("Failed to open output stream")?;

        Ok(Self {
            cap_stream,
            out_stream,
            filters: vec![],
            width: cap_fmt.width,
            height: cap_fmt.height,
        })
    }

    #[must_use]
    pub fn add_filter(mut self, filter: Box<dyn FrameFilter>) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn clear_filters(mut self) -> Self {
        self.filters.clear();
        self
    }

    pub fn process_frame(&mut self) -> anyhow::Result<()> {
        // Get the next frame
        let (buf_in, meta_in) =
            CaptureStream::next(&mut self.cap_stream).context("Failed to read capture frame")?;
        let (buf_out, meta_out) =
            OutputStream::next(&mut self.out_stream).context("Failed to read output frame")?;

        // Process the frame
        let mut buf = buf_in.to_vec();
        let mut frame = Frame::new(&mut buf, self.width, self.height);
        for filter in &self.filters {
            filter.process(&mut frame);
        }

        // Output the processed frame
        let buf_out = &mut buf_out[..buf_in.len()];
        buf_out.copy_from_slice(frame.as_bytes());

        // Set metadata
        // https://www.kernel.org/doc/html/v4.15/media/uapi/v4l/buffer.html#struct-v4l2-buffer
        meta_out.field = 0;
        meta_out.bytesused = meta_in.bytesused;

        Ok(())
    }
}
