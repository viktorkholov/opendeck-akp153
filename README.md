![Plugin Icon](assets/icon.png)

# OpenDeck Ajazz AKP153 / Mirabox HSV293S Plugin

An unofficial plugin for Mirabox HSV293S-family devices

> **Fork: `maddog-performance-optimization`.** This branch makes page switches faster on slow-to-write devices. It was tuned on a Maddog GK150K. It is not affiliated with upstream, so please don't report issues from this fork to [4ndv/opendeck-akp153](https://github.com/4ndv/opendeck-akp153).
>
> The changes were written with an AI coding assistant (Claude) and reviewed by the fork's owner.

## Results on a Maddog GK150K

| | Upstream 0.11.1 | This branch |
|---|---|---|
| Response to a key press | ~0.15 s, when the screen goes black | **~10 ms**: the pressed key flashes |
| Black screen on a page switch | ~0.6–0.8 s | **None**: the old page stays until the new one draws over it |
| Whole page on screen | ~0.8 s, all at once after the dark | `classic` ~0.55 s, `wave` ~0.7 s, `snake` ~1.2 s |
| Keys that look the same on both pages | Redrawn on every switch | Skipped, ~30 ms saved per key |
| Background key updates during a switch | Sent ahead of the page | Wait behind the page |

## Why page switches were slow

Measured with millisecond logs and direct HID writes on a GK150K (firmware 3.00, USB 2.0 high-speed):

| What the device does | Time |
|---|---|
| Takes one key image (85×85 JPEG), whatever its size | **~30 ms**. 15 images of 15 KB took 442 ms, and 15 of 30 KB took 475 ms |
| Refresh (STP), which shows what was uploaded | ~40 ms |
| Clear one key (CLE) | ~40 ms, unbatched |
| Any other command (brightness, keep-alive) | ~0.5 ms |

The device holds uploaded images until a refresh. USB is not the bottleneck: commands pass in half a millisecond. The cost is the number of images and refreshes.

Upstream on a page switch:

1. OpenDeck clears the whole screen, and the device goes black at once.
2. OpenDeck sends all 18 keys, including the ones that did not change.
3. The plugin waits 50 ms of quiet, then uploads them all with one refresh at the end.

The keys stay dark for most of a second, then the page appears all at once.

## What this branch changes

Everything lives in `src/fast.rs`, with hooks in `src/device.rs` and `src/main.rs`.

| | Upstream 0.11.1 | This branch |
|---|---|---|
| Image OpenDeck sends again unchanged | Re-encoded and re-sent | Skipped, because the key already shows it |
| Whole-screen clear on page switch | Sent at once, so the screen goes black | Ignored. The new page draws over the old one |
| Key press | No feedback until OpenDeck redraws | The pressed key brightens with a white frame in ~10 ms. If nothing replaces it, its own image comes back after 160 ms |
| Page drawing | One refresh at the end | In waves, a refresh after each, in the order of the chosen animation |
| Background updates during a page switch | Queued ahead of the page | The page takes over, and the rest wait behind it |
| Single-key clear | Its own command, ~40 ms each | A black image that joins the batch |
| Flush debounce | 50 ms | 15 ms. A page switch flushes as soon as all its keys are in, or after at most 300 ms |
| JPEG quality | 90 | 85 |
| Log | `Flushing pending updates` | `Flushed N images, B bytes in W waves, T ms` |

## Animations

The look is set in `~/Library/Application Support/opendeck/akp153-animation`, which is read on every flush, so a change applies on the next page switch. The file holds one order plus optional words. Without the file the default is `wave flash`.

| Order | How the page appears | Page time on the GK150K |
|---|---|---|
| `classic` (`batch`) | Everything at once, like upstream but without the black screen | ~0.55 s |
| `wave` (`diagonal`) | One diagonal at a time, top-left to bottom-right | ~0.7 s |
| `snake` | One key at a time down and up the columns: 0, 6, 12, 13, 7, 1, 2, 8, 14, 3, 9, 15, 16, 10, 4 | ~1.2 s |
| `bands` | The same diagonal in three waves | ~0.55 s |
| `rows` | Top to bottom | |
| `sweep` | Column by column from the pressed key | |
| `rings` | Rings around the pressed key | |
| `keys` | One key at a time, in position order | |

| Word | Effect |
|---|---|
| `flash` | Flash the pressed key |
| `dim` | Dim the screen to 12% during a page switch and ramp it back afterwards |
| `once` | One refresh at the end instead of one per wave. The page appears all at once, ~240 ms sooner |

Each wave costs a ~40 ms refresh, so more waves look smoother and take longer.

To switch animations from the deck itself, use a key that runs a shell command and writes the file, e.g. `echo "snake flash" > ~/Library/Application\ Support/opendeck/akp153-animation`.

## Tried and dropped

| Idea | Result |
|---|---|
| Lower JPEG quality (90 → 75) to send fewer bytes | Almost no effect, because the device's cost is per image, not per byte (442 vs 475 ms for half the bytes). Set to 85 to keep keys sharp |
| Keep the old page up until the whole new page was in, then show it at once | No black screen, but nothing happened for ~0.55 s after a press, which felt like lag. Replaced by the key flash and waves |
| Dim the screen during a switch (`dim`) | Works (~0.5 ms per brightness command), but looked worse than plain waves. Off by default |
| Three-band diagonal (`bands`) | ~160 ms faster than `wave`, but the wave looked less smooth |
| One refresh per page instead of per wave (`once`) | ~240 ms faster, but the device holds images until a refresh, so the wave disappears and the page pops in at once |
| "Pincer" animation (top rows left to right, bottom row right to left) | Looked messy. Removed |
| Start the switch on key down instead of key up | The GK150K sends one input report per press, with the state byte always 0, so there is no separate press and release to act on |
| Larger HID reports | The report size (512 bytes) is fixed by the device's descriptor |
| Shorter debounce (50 → 20 → 15 ms) | Kept, but it saves only a few ms. The device time dominates |
| Faster cable, hub, USB-C adapter | No effect possible. The device already runs at USB 2.0 high-speed, and commands take 0.5 ms |
| Firmware update | None published for this device, and the firmware is closed |
| Preload the next page into the device's buffer, then flip with one refresh | Should make switches ~50–100 ms, but the plugin would need to know the next page in advance (OpenDeck pages would have to be driven by a plugin). Not done, because the waves were preferred |

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
