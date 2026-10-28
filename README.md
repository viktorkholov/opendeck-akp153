![Plugin Icon](assets/icon.png)

# OpenDeck Ajazz AKP153 / Mirabox HSV293S Plugin

An unofficial plugin for Mirabox HSV293S-family devices

> **Fork: `maddog-performance-optimization`.** This branch makes page switches faster on slow-to-write devices. It was tuned on a Maddog GK150K. It is not affiliated with upstream, so please don't report issues from this fork to [4ndv/opendeck-akp153](https://github.com/4ndv/opendeck-akp153).
>
> The changes were written with an AI coding assistant (Claude) and reviewed by the fork's owner.

## Why page switches were slow

Measured on a Maddog GK150K (firmware 3.00, USB high-speed):

| Write | Time per 512-byte report |
|---|---|
| Image data | ~3.8 ms |
| Command (keep-alive, clear) | ~0.5 ms |

On a page switch, OpenDeck clears the whole screen and then sends all 18 keys. With upstream's quality-90 JPEGs that is about 40–55 KB, so the keys stay dark for most of a second and then the page appears all at once.

## What this branch changes

All of it lives in `src/fast.rs`, plus a few hooks in `src/device.rs`.

| | Upstream 0.11.1 | This branch |
|---|---|---|
| Image OpenDeck sends again unchanged | Re-encoded and re-sent | Skipped: the key already shows it |
| JPEG quality | 90 | 75, about a third fewer bytes |
| Whole-screen clear on page switch | Sent at once, so the screen goes dark | Held back: the old page stays until the new one has arrived, then they swap in one flush |
| Single-key clear | Its own command, ~40 ms each, unbatched | A black image that joins the batched flush |
| Flush debounce | 50 ms | 20 ms (a page switch waits up to 300 ms for all its keys) |
| Log | `Flushing pending updates` | `Flushed N images, B bytes in T ms` |

BMP-mode devices keep the upstream image path.

## Install over an existing plugin

1. Build it: `cargo build --release`.
2. Quit OpenDeck.
3. Copy `target/release/opendeck-akp153` over `opendeck-akp153-mac` in `~/Library/Application Support/opendeck/plugins/st.lynx.plugins.opendeck-akp153.sdPlugin/`. Keep the original so you can roll back.
4. Start OpenDeck.

## OpenDeck version

Requires OpenDeck 2.5.0 or newer

## Supported devices

- Mirabox HSV293S (5548:6670)
- Mirabox HSV293SV3 (6603:1014,6603:1005)
- Ajazz AKP153 (5548:6674)
- Ajazz AKP153E (0300:1010)
- Ajazz AKP153R (0300:1020)
- Ajazz AKP153E (rev. 2) (0300:3010)
- Ajazz AKP153R (rev. 2) (0300:3011)
- Mars Gaming MSD-ONE (0b00:1000,0b00:1005)
- Maddog GK150K (0c00:1000)
- Risemode Vision 01 (0a00:1001)
- TMICE Stream Controller (0500:1001)
- Soomfon Stream Controller XF-CN001 (1500:3003)
- Soomfon Studio Control Deck (5548:6670) (would be displayed as Mirabox HSV293S)
- Womier D15 (0600:1000)
- Monstargear MonstarDeck TS115 (0400:1000)
- Streonor Stream Controller Standard S15 (1500:3005)

## Platform support

- Linux: Guaranteed, if stuff breaks - I'll probably catch it before public release
- Mac: Best effort, no tests before release, things may break, but I probably have means to fix them
- Windows: Zero effort, no tests before release, if stuff breaks - too bad, it's up to you to contribute fixes

## Installation

1. Download an archive from [releases](https://github.com/4ndv/opendeck-akp153/releases)
2. In OpenDeck: Plugins -> Install from file
3. Linux: Download [udev rules](./40-opendeck-akp153.rules) and install them by copying into `/etc/udev/rules.d/` and running `sudo udevadm control --reload-rules`
4. Unplug and plug again the device, restart OpenDeck

## Known issues

- All the "old" devices come with the same serial number. You cannot use two of the same devices at the same time (for example a pair of 153R-s), but you can use two different devices at the same time (for example a 153R and a 153E)

## Building

### Prerequisites

You'll need:

- A Linux OS of some sort
- Rust 1.87 and up with `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-gnu` targets installed
- Docker
- [just](https://just.systems)

### Building a release package

```sh
$ just package
```

## Acknowledgments

This plugin is heavily based on work by contributors of [elgato-streamdeck](https://github.com/streamduck-org/elgato-streamdeck) crate
