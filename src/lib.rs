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
use std::ops::Index;

use anyhow::{anyhow, Context};

use itertools::Itertools;

use rayon::prelude::*;

use rusttype::{point, Font, Scale, ScaledGlyph};

use v4l::{
    buffer::Type,
    format::fourcc::FourCC,
    io::traits::{CaptureStream, OutputStream},
    prelude::*,
    video::{Capture, Output},
};

// TODO: Add more options
// $@B%8&WM#*oahkbdpqwmZO0QLCJUYXzcvunxrjft/\|()1{}[]?-_+~<>i!lI;:,"^`'.
pub const ASCII_MAP_64: [char; 64] = [
    '$', '@', 'B', '%', '&', 'W', 'M', '#', '*', 'o', 'h', 'k', 'b', 'd', 'p', 'q', 'w', 'm', 'Z',
    '0', 'Q', 'L', 'C', 'J', 'U', 'Y', 'X', 'z', 'c', 'u', 'n', 'x', 'r', 'j', 'f', '/', '\\', '|',
    '(', ')', '1', '{', '}', '[', ']', '?', '-', '_', '+', '~', '<', '>', 'i', '!', 'I', ';', ':',
    ',', '"', '^', '`', '\'', '.', ' ',
];
// TODO: Make this an argument
pub const FONT: &[u8] = include_bytes!("../font/FiraCode-VF.ttf");
// TODO: Figure out good values for these
pub const FONT_SCALE: f32 = 20.0;
const AVG_GROUP_SIZE: u32 = 10;
const NSUBFRAMES: u32 = 4;

#[derive(Debug)]
pub struct RenderedGlyph(Vec<(u32, u32, Brightness)>);

impl RenderedGlyph {
    #[must_use]
    pub fn new(glyph: ScaledGlyph<'_>) -> Self {
        let mut pts = vec![];
        glyph.positioned(point(0.0, 0.0)).draw(|x, y, v| {
            pts.push((x, y, Brightness::from(v)));
        });
        Self(pts)
    }
}

#[derive(Debug)]
pub struct GlyphMap(HashMap<char, RenderedGlyph>);

impl GlyphMap {
    pub fn new(font: &'static [u8], scale: f32, chars: &[char]) -> anyhow::Result<Self> {
        let font = Font::try_from_bytes(font).context("Failed to load font")?;
        let scale = Scale::uniform(scale);
        Ok(Self(
            chars
                .iter()
                .map(|&c| (c, RenderedGlyph::new(font.glyph(c).scaled(scale))))
                .collect(),
        ))
    }
}

#[derive(Debug)]
pub struct AsciiMap {
    map: &'static [char],
    nbits: u32,
}

impl AsciiMap {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    #[must_use]
    pub fn new(map: &'static [char]) -> Self {
        let nbits = (map.len() as f32).log2() as u32;
        debug_assert!(
            2usize.pow(nbits) == map.len(),
            "len={}, nbits={}",
            map.len(),
            nbits
        );
        debug_assert!(nbits <= u8::BITS, "nbits={}", nbits);
        Self {
            map,
            nbits: (map.len() as f32).log2() as u32,
        }
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

#[derive(Debug, Clone, Copy)]
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
        debug_assert!(self.height > 1);
        debug_assert!(n > 0);
        let mut len = self.buf.len() / 2;
        let mut height = self.height / 2;

        let (sub1, sub2) = self.buf.split_at_mut(len);
        let mut bufs = vec![sub1, sub2];
        for _ in 1..n {
            let mut new_bufs = vec![];
            len /= 2;
            height /= 2;
            for buf in bufs {
                let (subbuf1, subbuf2) = buf.split_at_mut(len);
                new_bufs.push(subbuf1);
                new_bufs.push(subbuf2);
            }
            bufs = new_bufs;
        }
        bufs.into_iter()
            .map(|buf| Yuyv::new(buf, self.width, height))
            .collect()
    }

    #[must_use]
    pub const fn iter_avg(&self, group_sz: u32) -> IterAvg<'_> {
        IterAvg::new(self, group_sz)
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
    group_sz: u32,
    x: u32,
    y: u32,
    done: bool,
}

impl<'pix> IterAvg<'pix> {
    #[must_use]
    const fn new(pixels: &'pix Yuyv<'pix>, group_sz: u32) -> Self {
        Self {
            pixels,
            group_sz,
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
            let next_x = cmp::min(self.x + self.group_sz, self.pixels.width);
            let next_y = cmp::min(self.y + self.group_sz, self.pixels.height);
            let npix = (next_x - self.x) * (next_y - self.y);
            let avg = (self.x..next_x)
                .cartesian_product(self.y..next_y)
                .map(|(x, y)| u32::from(self.pixels.get_brightness(x, y).0))
                .sum::<u32>()
                / npix;
            debug_assert!(avg <= u8::MAX.into(), "avg={}", avg);
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
    fn process<'pix>(&self, frame: &mut Frame<'pix>);
}

// TODO: Add option for grayscale/color
pub struct AsciiFilter<'ascii, 'glyph> {
    ascii_map: &'ascii AsciiMap,
    glyphs: &'glyph GlyphMap,
}

impl<'ascii, 'glyph> AsciiFilter<'ascii, 'glyph> {
    #[must_use]
    pub const fn new(ascii_map: &'ascii AsciiMap, glyphs: &'glyph GlyphMap) -> Self {
        Self { ascii_map, glyphs }
    }
}

impl FrameFilter for AsciiFilter<'_, '_> {
    fn process<'pix>(&self, frame: &mut Frame<'pix>) {
        let mut buf = frame.as_bytes().to_vec();
        let mut old_frame = Frame::new(&mut buf, frame.width(), frame.height());
        let mut subframes = frame.splitn(NSUBFRAMES);
        let old_subframes = old_frame.splitn(NSUBFRAMES);
        subframes
            .par_iter_mut()
            .zip(old_subframes)
            .for_each(|(subframe, old_subframe)| {
                let width = old_subframe.width();
                let height = old_subframe.height();
                old_subframe
                    .pixels
                    .iter_avg(AVG_GROUP_SIZE)
                    .flat_map(|(x, y, pix)| {
                        self.glyphs
                            .0
                            .get(&pix.as_ascii(self.ascii_map))
                            .unwrap()
                            .0
                            .iter()
                            .filter_map(move |(xoff, yoff, b)| {
                                let x = x + xoff;
                                let y = y + yoff;
                                (x < width && y < height).then(|| (x, y, b))
                            })
                    })
                    .for_each(|(x, y, b)| subframe.pixels.set_brightness(x, y, *b));
            });
    }
}

pub struct StreamProcessor<'cap, 'out, 'filt, F> {
    cap_stream: MmapStream<'cap>,
    out_stream: MmapStream<'out>,
    filters: Vec<&'filt F>,
    width: u32,
    height: u32,
}

impl<'filt, F> StreamProcessor<'_, '_, 'filt, F>
where
    F: FrameFilter,
{
    pub fn new(source: &str, sink: &str) -> anyhow::Result<Self> {
        println!(
            "Using source device: {}\nUsing sink device: {}\n",
            source, sink
        );

        // Prepare capture and output devices
        let cap = Device::with_path(source).context("Failed to open capture device")?;
        let out = Device::with_path(sink).context("Failed to open output device")?;

        // Confirm capture and output settings match and are valid
        let mut cap_fmt = Capture::format(&cap).context("Failed to read capture format")?;
        cap_fmt.fourcc = FourCC::new(b"YUYV");
        let cap_fmt =
            Capture::set_format(&cap, &cap_fmt).context("Failed to set capture format")?;
        let out_fmt = Output::set_format(&out, &cap_fmt).context("Failed to set output format")?;

        if cap_fmt.fourcc.str()? != "YUYV" {
            return Err(anyhow!("Invalid fourcc: {}", cap_fmt.fourcc.str().unwrap()));
        }

        if cap_fmt.width != out_fmt.width
            || cap_fmt.height != out_fmt.height
            || cap_fmt.fourcc != out_fmt.fourcc
        {
            return Err(anyhow!(
                "Output format does not match capture:\nCapture format: {}\nOutput format: {}",
                cap_fmt,
                out_fmt
            ));
        }

        println!(
            "Capture device:\n{}{}{}\nOutput device:\n{}{}{}",
            cap.query_caps()
                .context("Failed to read capture capabilities")?,
            Capture::format(&cap).context("Failed to read capture format")?,
            Capture::params(&cap).context("Failed to read capture parameters")?,
            out.query_caps()
                .context("Failed to read output capabilities")?,
            Output::format(&out).context("Failed to read output format")?,
            Output::params(&out).context("Failed to read output parameters")?
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
    pub fn add_filter(mut self, filter: &'filt F) -> Self {
        self.filters.push(filter);
        self
    }

    pub fn process_frame(&mut self) -> anyhow::Result<()> {
        // Get the next frame
        let (buf_in, _) =
            CaptureStream::next(&mut self.cap_stream).context("Failed to read capture frame")?;
        let (buf_out, _) =
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
        Ok(())
    }
}
