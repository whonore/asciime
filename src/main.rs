use std::cmp;
use std::collections::HashMap;
use std::ops::Index;

use anyhow::{anyhow, Context};

use itertools::Itertools;

use rusttype::{point, Font, Scale, ScaledGlyph};

use v4l::{
    buffer::Type,
    format::fourcc::FourCC,
    io::traits::{CaptureStream, OutputStream},
    prelude::*,
    video::{Capture, Output},
};

// $@B%8&WM#*oahkbdpqwmZO0QLCJUYXzcvunxrjft/\|()1{}[]?-_+~<>i!lI;:,"^`'.
const ASCII_MAP_64: [char; 64] = [
    '$', '@', 'B', '%', '&', 'W', 'M', '#', '*', 'o', 'h', 'k', 'b', 'd', 'p', 'q', 'w', 'm', 'Z',
    '0', 'Q', 'L', 'C', 'J', 'U', 'Y', 'X', 'z', 'c', 'u', 'n', 'x', 'r', 'j', 'f', '/', '\\', '|',
    '(', ')', '1', '{', '}', '[', ']', '?', '-', '_', '+', '~', '<', '>', 'i', '!', 'I', ';', ':',
    ',', '"', '^', '`', '\'', '.', ' ',
];

const FONT_SCALE: f32 = 20.0;
const AVG_GROUP_SIZE: u32 = 10;

type GlyphMap = HashMap<char, ScaledGlyph<'static>>;

#[derive(Debug)]
struct AsciiMap {
    map: &'static [char],
    nbits: u32,
}

impl AsciiMap {
    fn new(map: &'static [char]) -> Self {
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

    fn index(&self, idx: Brightness) -> &Self::Output {
        let idx = idx.0 >> (u8::BITS - self.nbits);
        &self.map[idx as usize]
    }
}

#[derive(Debug, Clone, Copy)]
struct Brightness(u8);

impl Brightness {
    fn as_ascii(&self, map: &AsciiMap) -> char {
        map[*self]
    }
}

impl From<f32> for Brightness {
    fn from(b: f32) -> Self {
        // debug_assert!((0.0..=1.0).contains(&b), "b={}", &b);
        Self((b * 255.0).clamp(0.0, 255.0) as u8)
    }
}

#[derive(Debug, Clone)]
struct Yuyv {
    buf: Vec<u8>,
    width: u32,
    height: u32,
}

impl Yuyv {
    fn new(buf: Vec<u8>, width: u32, height: u32) -> Self {
        Self { buf, width, height }
    }

    fn iter_avg(&self, group_sz: u32) -> IterAvg {
        IterAvg::new(self, group_sz)
    }

    fn get_brightness(&self, x: u32, y: u32) -> Brightness {
        Brightness(self.buf[self.xy_to_idx(x, y)])
    }

    fn set_brightness<B>(&mut self, x: u32, y: u32, b: B)
    where
        B: Into<Brightness>,
    {
        let idx = self.xy_to_idx(x, y);
        if let Some(b_old) = self.buf.get_mut(idx) {
            *b_old = b.into().0;
        }
    }

    // https://egeeks.github.io/kernal/media/V4L2-PIX-FMT-YUYV.html
    // 0       1       2       3
    // Y1 U1/2 Y2 V1/2 Y3 U3/4 Y4 V3/4      0
    // Y5 U5/6 Y6 V5/6 Y7 U7/8 Y8 V7/8      1
    fn xy_to_idx(&self, x: u32, y: u32) -> usize {
        (2 * (y * self.width + x)) as usize
    }
}

#[derive(Debug)]
struct IterAvg<'pix> {
    pixels: &'pix Yuyv,
    group_sz: u32,
    x: u32,
    y: u32,
    done: bool,
}

impl<'pix> IterAvg<'pix> {
    fn new(pixels: &'pix Yuyv, group_sz: u32) -> Self {
        Self {
            pixels,
            group_sz,
            x: 0,
            y: 0,
            done: false,
        }
    }
}

impl<'pix> Iterator for IterAvg<'pix> {
    type Item = (u32, u32, Brightness);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            let next_x = cmp::min(self.x + self.group_sz, self.pixels.width);
            let next_y = cmp::min(self.y + self.group_sz, self.pixels.height);
            let npix = (next_x - self.x) * (next_y - self.y);
            let avg = (self.x..next_x)
                .cartesian_product(self.y..next_y)
                .map(|(x, y)| self.pixels.get_brightness(x, y).0 as u32)
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

#[derive(Debug, Clone)]
struct Frame<'ascii, 'glyph> {
    pixels: Yuyv,
    ascii_map: &'ascii AsciiMap,
    glyphs: &'glyph GlyphMap,
}

impl<'ascii, 'glyph> Frame<'ascii, 'glyph> {
    fn new(
        buf: Vec<u8>,
        width: u32,
        height: u32,
        ascii_map: &'ascii AsciiMap,
        glyphs: &'glyph GlyphMap,
    ) -> Self {
        Self {
            pixels: Yuyv::new(buf, width, height),
            ascii_map,
            glyphs,
        }
    }

    fn process(&self) -> Self {
        let mut frame = self.clone();
        self.pixels
            .iter_avg(AVG_GROUP_SIZE)
            .map(|(x, y, pix)| {
                (
                    x,
                    y,
                    self.glyphs
                        .get(&pix.as_ascii(self.ascii_map))
                        .cloned()
                        .unwrap()
                        .positioned(point(x as f32, y as f32)),
                )
            })
            .for_each(|(xmin, ymin, glyph)| {
                glyph.draw(|x, y, v| {
                    frame.pixels.set_brightness(x + xmin, y + ymin, v);
                })
            });
        frame
    }

    fn as_bytes(&self) -> &[u8] {
        &self.pixels.buf
    }
}

fn main() -> anyhow::Result<()> {
    let ascii_map = AsciiMap::new(&ASCII_MAP_64);

    // Load font map
    let scale = Scale::uniform(FONT_SCALE);
    let font_data = include_bytes!("../font/FiraCode-VF.ttf");
    let font = Font::try_from_bytes(font_data).context("Failed to load font")?;
    let glyphs: GlyphMap = ASCII_MAP_64
        .iter()
        .map(|&c| (c, font.glyph(c).scaled(scale)))
        .collect();

    // Prepare capture and output devices
    let source = "/dev/video0";
    let sink = "/dev/video4";
    println!(
        "Using source device: {}\nUsing sink device: {}\n",
        source, sink
    );

    let cap = Device::with_path(source).context("Failed to open capture device")?;
    let out = Device::with_path(sink).context("Failed to open output device")?;

    // Confirm capture and output settings match and are valid
    let mut cap_fmt = Capture::format(&cap).context("Failed to read capture format")?;
    cap_fmt.fourcc = FourCC::new(b"YUYV");
    let cap_fmt = Capture::set_format(&cap, &cap_fmt).context("Failed to set capture format")?;
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
        "Capture device:\n{}{}{}",
        cap.query_caps()
            .context("Failed to read capture capabilities")?,
        Capture::format(&cap).context("Failed to read capture format")?,
        Capture::params(&cap).context("Failed to read capture parameters")?
    );

    println!(
        "Output device:\n{}{}{}",
        out.query_caps()
            .context("Failed to read output capabilities")?,
        Output::format(&out).context("Failed to read output format")?,
        Output::params(&out).context("Failed to read output parameters")?
    );

    // Prepare capture and output streams
    let mut cap_stream =
        MmapStream::new(&cap, Type::VideoCapture).context("Failed to open capture stream")?;
    let mut out_stream =
        MmapStream::new(&out, Type::VideoOutput).context("Failed to open output stream")?;

    CaptureStream::next(&mut cap_stream).context("Failed to read capture frame")?;
    loop {
        // Get the next frame
        let (buf_in, _) =
            CaptureStream::next(&mut cap_stream).context("Failed to read capture frame")?;
        let (buf_out, _) =
            OutputStream::next(&mut out_stream).context("Failed to read output frame")?;

        // Process the frame
        let frame = Frame::new(
            buf_in.to_vec(),
            cap_fmt.width,
            cap_fmt.height,
            &ascii_map,
            &glyphs,
        )
        .process();

        // Output the processed frame
        let buf_out = &mut buf_out[..buf_in.len()];
        buf_out.copy_from_slice(frame.as_bytes());
    }
}
