![Plugin Icon](assets/icon.png)

# OpenDeck Ajazz AKP153 / Mirabox HSV293S Plugin

An unofficial plugin for Mirabox HSV293S-family devices

> **Fork: `maddog-performance-optimization`.** This branch makes page switches faster on slow-to-write devices. It was tuned on a Maddog GK150K. It is not affiliated with upstream, so please don't report issues from this fork to [4ndv/opendeck-akp153](https://github.com/4ndv/opendeck-akp153).
>
> The changes were written with an AI coding assistant (Claude) and reviewed by the fork's owner.

## Results on a Maddog GK150K

| | Upstream 0.11.1 | This branch |
|---|---|---|
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
| Page drawing | One refresh at the end | In waves, a refresh after each. Pages and folders have separate animations, and a folder closes with its opening played backwards |
| Background updates during a page switch | Queued ahead of the page | The page takes over, and the rest wait behind it |
| Single-key clear | Its own command, ~40 ms each | A black image that joins the batch |
| Flush debounce | 50 ms | 15 ms. A page switch flushes as soon as all its keys are in, or after at most 300 ms |
| JPEG quality | 90 | 85 |
| Log | `Flushing pending updates` | `Flushed N images, B bytes in W waves, T ms` |

## Animations

Each kind of page switch has its own animation:

| Kind | When | Animation |
|---|---|---|
| `page` | The nav key switches pages | Set by `page`. Default `wave` |
| `open` | Any other key switched pages: a folder opens | Set by `open`. Default `rsnake` |
| `close` | The nav key is pressed inside a folder (a back key) | Always the opening played backwards: `rsnake` → `snake`, `wave` → `rwave`. `classic` stays `classic` |

The driver treats the deck as three layers:

- **Controller:** the nav key, pages or back.
- **Side screen:** the last column.
- **Content:** everything else.

Controller and side screen are never blanked or animated. A key there is redrawn only when its image changes, so it stays lit through every switch. Only content keys animate. A folder opens in one pass over the content: each key goes straight to its final image, either the folder's key or a dark tile where the folder is empty.

The driver knows which key was pressed, so it tells the switch kinds apart by itself. It also counts open folders, so nested folders close back one level at a time. Any key that switches pages counts as a folder, with nothing to set per folder.

Settings live in `~/Library/Application Support/opendeck/akp153-animation`. The file is read on every switch, so a change applies to the next one:

```text
page  wave       # animation for the nav key switching pages
open  rsnake     # animation when a folder opens; it closes with the same played backwards
nav   16         # the nav key (OpenDeck position); without it every switch is a page
```

A file with a single line in the old format, e.g. `wave`, sets the page animation.

| Animation | How the page appears | Time on the GK150K |
|---|---|---|
| `classic` | Everything at once, like upstream but without the black screen | ~0.55 s |
| `wave` | One diagonal at a time, top-left to bottom-right | ~0.7 s |
| `rwave` | The diagonal backwards, from the bottom right | ~0.7 s |
| `snake` | One key at a time down and up the columns: 0, 6, 12, 13, 7, 1, 2, 8, 14, 3, 9, 15, 16, 10, 4 | ~1.2 s |
| `rsnake` | The snake backwards. Each key goes straight to its final image, so a half-empty folder looks whole | ~1 s for 14 keys |

Each wave costs a ~40 ms refresh, so more waves look smoother and take longer.

To change animations from the deck itself, use keys that run a shell command to rewrite the file.

## Tried and dropped

| Idea | Result |
|---|---|
| Lower JPEG quality (90 → 75) to send fewer bytes | Almost no effect, because the device's cost is per image, not per byte (442 vs 475 ms for half the bytes). Set to 85 to keep keys sharp |
| Keep the old page up until the whole new page was in, then show it at once | No black screen, but nothing happened for ~0.55 s after a press, which felt like lag. Replaced by waves |
| Dim the screen during a switch | Works (~0.5 ms per brightness command), but looked worse than plain waves. Removed |
| Folder opening as "darken all content, then draw" | The dark phase took ~0.45 s before anything new showed, and in a half-empty folder little was left to animate afterwards. Replaced by `rsnake` |
| Flash the pressed key (brightened, white frame, ~10 ms) | Gave instant feedback, but the device's firmware already gives its own press feedback, so it only cost an extra image per switch. Removed |
| Three-band diagonal | ~160 ms faster than `wave`, but the wave looked less smooth. Removed |
| One refresh per page instead of per wave | ~240 ms faster, but the device holds images until a refresh, so the wave disappears and the page pops in at once |
| "Pincer" animation (top rows left to right, bottom row right to left) | Looked messy. Removed |
| More orders: `bloom` (backdrop first, then rings out of the folder key), `fold`, `rings`, `sweep`, `rows`, `keys` | Tried for folders and pages. None looked better than the snake and wave pair, so they were removed to keep the driver simple |
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
