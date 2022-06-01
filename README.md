# ASCIIMe

An ASCII art webcam filter.

## Quickstart

- Install [v4l2loopback](https://github.com/umlaeute/v4l2loopback/)
- Load the v4l2loopback kernel module
```shell
sudo modprobe v4l2loopback -v video_nr=11 card_label="AsciiMe" exclusive_caps=1 max_buffers=2
```
- Run AsciiMe
```shell
cargo run --release /dev/video0 /dev/video11
```
