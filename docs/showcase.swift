// Renders the README's showcase GIFs: a simulated 3x6 deck switching pages, timed with what was
// measured on a Maddog GK150K. It is a simulation, not a screen recording.
//
//   swift docs/showcase.swift docs
//
// Timing model (ms after the key press), fitted to the driver's own log ("ms after the press"):
//   the fork starts sending ~85 after the press; each key image takes ~30 and each refresh (STP),
//   which shows what was sent, ~41. Keys appear when their refresh is done. That gives wave 0.75 s
//   and snake 1.08 s, the medians measured from the press to the complete page.
//   Upstream clears the whole screen at ~20 and shows the new page ~1.25 s after the press, as
//   measured on our deck: pages with empty keys made it clear each of them on its own first.
import AppKit
import ImageIO
import UniformTypeIdentifiers

let out = CommandLine.arguments.dropFirst().first ?? "docs"

/// Part of every GIF's file name: GitHub caches README images by URL, so a new version needs a new name.
let VERSION = "v3"

/// Pixels per point, so the images stay sharp when the README shows them large.
let SCALE: CGFloat = 2

/// A bitmap `size` points large at SCALE, with drawing set up in points.
func canvas(_ size: CGSize, _ draw: () -> Void) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width * SCALE), pixelsHigh: Int(size.height * SCALE),
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let transform = NSAffineTransform()
    transform.scale(by: SCALE)
    transform.concat()
    draw()
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

// MARK: model

enum Tile: Equatable {
    case number(Int, CGFloat) // label, hue
    case empty
    case nav(String, String) // SF Symbol, label
    case folder(CGFloat)
}

typealias Page = [Int: Tile] // OpenDeck positions 0...17; column 5 is the side screen

let side = [5, 11, 17]
let controller = 16
let snake = [0, 6, 12, 13, 7, 1, 2, 8, 14, 3, 9, 15, 16, 10, 4]

/// A desktop of numbered tiles whose hue runs from `hues.0` to `hues.1` across the keys.
func desktop(_ hues: (CGFloat, CGFloat), folderAt: Int? = nil) -> Page {
    var page = Page()
    var n = 1
    for p in 0..<18 where !side.contains(p) && p != controller {
        let hue = hues.0 + (hues.1 - hues.0) * CGFloat(n - 1) / 13
        page[p] = p == folderAt ? .folder(0.74) : .number(n, hue.truncatingRemainder(dividingBy: 1))
        n += 1
    }
    page[controller] = .nav("arrow.left.arrow.right", "pages")
    return page
}

let desktopA = desktop((1.07, 0.74), folderAt: 3) // warm: peach, pink, lavender
let desktopB = desktop((0.42, 0.68))                 // cool: mint, sky, indigo
let folder: Page = {
    var page = Page()
    for p in 0..<18 where !side.contains(p) && p != controller { page[p] = .empty }
    page[0] = .number(1, 0.12); page[1] = .number(2, 0.14); page[6] = .number(3, 0.16) // butter yellow
    page[controller] = .nav("chevron.left", "back")
    return page
}()

/// When each key changes to its new tile, and when the screen goes black (upstream only).
struct Timeline {
    var change: [Int: Double] = [:]
    var black: (from: Double, to: Double)? = nil
    var dark: [Int: (from: Double, to: Double)] = [:] // keys shown dark for a while (the dropped "vanish")
    var end: Double { max(change.values.max() ?? 0, black?.to ?? 0) }
}

func upstream(_ from: Page, _ to: Page) -> Timeline {
    let done = 1250.0
    var t = Timeline(black: (20, done))
    for p in 0..<18 { t.change[p] = done }
    return t
}

/// The fork: unchanged keys are skipped; controller and side go first; content in waves.
func fork(_ from: Page, _ to: Page, order: String, vanish: Bool = false) -> Timeline {
    var t = Timeline()
    var clock = 85.0
    let changed = (0..<18).filter { from[$0] != to[$0] }
    let chrome = changed.filter { side.contains($0) || $0 == controller }
    var content = changed.filter { !chrome.contains($0) }
    func wave(_ keys: [Int]) {
        guard !keys.isEmpty else { return }
        clock += Double(keys.count) * 30 + 41 // the images, then the refresh that shows them
        for p in keys { t.change[p] = clock }
    }
    if vanish { // darken every content key first, then draw the folder's own keys
        let all = (0..<18).filter { !side.contains($0) && $0 != controller }
        let darkAt = clock + Double(all.count + chrome.count) * 30 + 41
        for p in all { t.dark[p] = (darkAt, darkAt) }
        for p in chrome { t.change[p] = darkAt }
        for p in all where to[p] == .empty { t.change[p] = darkAt }
        clock = darkAt
        content = content.filter { to[$0] != .empty }
        for p in content { t.dark[p] = (darkAt, .infinity) }
        wave(content)
        for p in content { t.dark[p]!.to = t.change[p]! }
        return t
    }
    wave(chrome)
    let rc = { (p: Int) in (p / 6, p % 6) }
    let key: (Int) -> Int = { p in
        let (r, c) = rc(p)
        switch order {
        case "classic": return 0
        case "wave": return r + c
        case "snake": return snake.firstIndex(of: p) ?? 99
        case "rsnake": return snake.count - (snake.firstIndex(of: p) ?? 99)
        default: return 0
        }
    }
    for k in Set(content.map(key)).sorted() { wave(content.filter { key($0) == k }) }
    return t
}

// MARK: drawing

let K: CGFloat = 64, GAP: CGFloat = 9, PAD: CGFloat = 18, HEADER: CGFloat = 34
let deckW = PAD * 2 + 5 * K + 4 * GAP + 14 + K * 0.9
let deckH = HEADER + PAD + 3 * K + 2 * GAP + PAD

func hex(_ h: UInt32, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((h >> 16) & 255) / 255, green: CGFloat((h >> 8) & 255) / 255, blue: CGFloat(h & 255) / 255, alpha: a)
}

func text(_ s: String, _ size: CGFloat, _ color: NSColor, at p: CGPoint, weight: NSFont.Weight = .semibold, center: Bool = false) {
    let a = NSAttributedString(string: s, attributes: [.font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color])
    a.draw(at: CGPoint(x: center ? p.x - a.size().width / 2 : p.x, y: p.y))
}

func keyRect(_ p: Int, origin: CGPoint) -> CGRect {
    let r = p / 6, c = p % 6
    return CGRect(x: origin.x + PAD + CGFloat(c) * (K + GAP), y: origin.y + PAD + CGFloat(2 - r) * (K + GAP), width: K, height: K)
}

func draw(_ tile: Tile?, in rect: CGRect) {
    let shape = NSBezierPath(roundedRect: rect, xRadius: 10, yRadius: 10)
    switch tile {
    case .number(let n, let hue):
        // A negative hue is a grey tile, for the layers map, where the outlines must stand out.
        let top = hue < 0 ? hex(0x3f3f46) : NSColor(hue: hue, saturation: 0.38, brightness: 1, alpha: 1)
        let bottom = hue < 0 ? hex(0x2f2f35) : NSColor(hue: hue, saturation: 0.55, brightness: 0.93, alpha: 1)
        NSGradient(starting: top, ending: bottom)!.draw(in: shape, angle: -90)
        text("\(n)", 30, hue < 0 ? hex(0xffffff, 0.35) : hex(0x1e1b4b, 0.8), at: CGPoint(x: rect.midX, y: rect.midY - 19), weight: .heavy, center: true)
    case .folder(let hue):
        (hue < 0 ? hex(0x2f2f35) : NSColor(hue: hue, saturation: 0.5, brightness: 0.45, alpha: 1)).setFill(); shape.fill()
        if let img = NSImage(systemSymbolName: "folder.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 26, weight: .semibold).applying(.init(paletteColors: [hex(0xddd6fe)]))) {
            img.draw(in: CGRect(x: rect.midX - img.size.width / 2, y: rect.midY - img.size.height / 2 + 4, width: img.size.width, height: img.size.height))
        }
        text("folder", 11, hex(0xffffff, 0.7), at: CGPoint(x: rect.midX, y: rect.minY + 5), center: true)
    case .nav(let symbol, let label):
        hex(0x26262b).setFill(); shape.fill()
        if let img = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 20, weight: .bold).applying(.init(paletteColors: [.white]))) {
            img.draw(in: CGRect(x: rect.midX - img.size.width / 2, y: rect.midY - img.size.height / 2 + 7, width: img.size.width, height: img.size.height))
        }
        text(label, 11, hex(0xffffff, 0.7), at: CGPoint(x: rect.midX, y: rect.minY + 5), center: true)
    case .empty:
        hex(0x141416).setFill(); shape.fill()
    case nil:
        hex(0x050505).setFill(); shape.fill()
    }
}

func drawSide(origin: CGPoint, lit: Bool) {
    let x = origin.x + PAD + 5 * (K + GAP) + 5
    let rect = CGRect(x: x, y: origin.y + PAD, width: K * 0.9, height: 3 * K + 2 * GAP)
    (lit ? hex(0x1c1c20) : hex(0x050505)).setFill()
    NSBezierPath(roundedRect: rect, xRadius: 10, yRadius: 10).fill()
    guard lit else { return }
    for (i, v) in [0.41, 0.15, 0.63].enumerated() {
        let c = CGPoint(x: rect.midX, y: rect.maxY - K / 2 - CGFloat(i) * (K + GAP))
        let track = NSBezierPath(); track.appendArc(withCenter: c, radius: 20, startAngle: 0, endAngle: 360); track.lineWidth = 5
        hex(0xffffff, 0.12).setStroke(); track.stroke()
        let arc = NSBezierPath(); arc.appendArc(withCenter: c, radius: 20, startAngle: 90, endAngle: 90 - 360 * v, clockwise: true); arc.lineWidth = 5
        hex(0x22c55e).setStroke(); arc.stroke()
    }
}

/// One deck at time `t` (ms after the press).
func drawDeck(_ title: String, from: Page, to: Page, timeline tl: Timeline, press: Int, t: Double, origin: CGPoint) {
    hex(0x0c0c0e).setFill()
    NSBezierPath(roundedRect: CGRect(x: origin.x, y: origin.y, width: deckW, height: deckH - HEADER), xRadius: 18, yRadius: 18).fill()
    let black = tl.black.map { t >= $0.from && t < $0.to } ?? false
    for p in 0..<18 where !side.contains(p) {
        let rect = keyRect(p, origin: origin)
        if black { draw(nil, in: rect); continue }
        if let d = tl.dark[p], t >= d.from, t < d.to { draw(nil, in: rect); continue }
        draw(t >= (tl.change[p] ?? .infinity) ? to[p] : from[p], in: rect)
    }
    drawSide(origin: origin, lit: !black)
    if t >= 0 && t < 160 { // the press
        let ring = NSBezierPath(roundedRect: keyRect(press, origin: origin).insetBy(dx: -3, dy: -3), xRadius: 12, yRadius: 12)
        ring.lineWidth = 3; hex(0xffffff, 0.9).setStroke(); ring.stroke()
    }
    text(title, 15, hex(0xffffff, 0.9), at: CGPoint(x: origin.x + 4, y: origin.y + deckH - HEADER + 8))
    guard press >= 0 else { return } // a still, without a clock
    let clock = t < 0 ? "press" : String(format: "%.2f s", min(t, tl.end) / 1000)
    let done = t >= tl.end
    text(done ? "\(clock) ✓" : clock, 15, done ? hex(0x4ade80) : hex(0xffffff, 0.6),
         at: CGPoint(x: origin.x + deckW - 80, y: origin.y + deckH - HEADER + 8), weight: .medium)
}

struct Scene { let title: String; let from: Page; let to: Page; let timeline: Timeline; let press: Int }

/// A GIF of one or more decks stacked, sharing one clock; several steps play one after another.
func gif(_ name: String, _ steps: [[Scene]]) {
    let rows = steps[0].count
    let size = CGSize(width: deckW + 24, height: CGFloat(rows) * (deckH + 12) + 12)
    let url = URL(fileURLWithPath: "\(out)/\(name)-\(VERSION).gif")
    var frames: [CGImage] = []
    var delays: [Double] = []
    for scenes in steps {
        let end = scenes.map { $0.timeline.end }.max()!
        var t = -400.0
        while t <= end + 900 {
            let rep = canvas(size) {
                hex(0x18181b).setFill(); CGRect(origin: .zero, size: size).fill()
                for (i, s) in scenes.enumerated() {
                    let y = size.height - CGFloat(i + 1) * (deckH + 12)
                    drawDeck(s.title, from: s.from, to: s.to, timeline: s.timeline, press: s.press, t: t, origin: CGPoint(x: 12, y: y))
                }
            }
            frames.append(rep.cgImage!)
            let step = t < 0 || t > end ? 100.0 : 20.0
            delays.append(step / 1000)
            t += step
        }
    }
    let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.gif.identifier as CFString, frames.count, nil)!
    CGImageDestinationSetProperties(dest, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFLoopCount: 0]] as CFDictionary)
    for (frame, delay) in zip(frames, delays) {
        CGImageDestinationAddImage(dest, frame, [kCGImagePropertyGIFDictionary: [
            kCGImagePropertyGIFDelayTime: delay, kCGImagePropertyGIFUnclampedDelayTime: delay]] as CFDictionary)
    }
    CGImageDestinationFinalize(dest)
    print("\(name)-\(VERSION).gif: \(frames.count) frames")
}

// MARK: the showcase

gif("upstream", [[Scene(title: "upstream 0.11.1", from: desktopA, to: desktopB,
                         timeline: upstream(desktopA, desktopB), press: controller)]])
for order in ["classic", "wave", "snake"] {
    gif("page-\(order)", [[Scene(title: "this fork · \(order)", from: desktopA, to: desktopB,
                                 timeline: fork(desktopA, desktopB, order: order), press: controller)]])
}
gif("folder", [
    [Scene(title: "open folder · rsnake", from: desktopA, to: folder, timeline: fork(desktopA, folder, order: "rsnake"), press: 3)],
    [Scene(title: "back · snake (mirrored)", from: folder, to: desktopA, timeline: fork(folder, desktopA, order: "snake"), press: controller)],
])
gif("folder-dropped", [[
    Scene(title: "dropped: darken, then draw", from: desktopA, to: folder,
          timeline: fork(desktopA, folder, order: "classic", vanish: true), press: 3),
    Scene(title: "kept: rsnake", from: desktopA, to: folder, timeline: fork(desktopA, folder, order: "rsnake"), press: 3),
]])
gif("tour", [
    [Scene(title: "Desktop 1 → Desktop 2 · pages", from: desktopA, to: desktopB, timeline: fork(desktopA, desktopB, order: "wave"), press: controller)],
    [Scene(title: "Desktop 2 → Desktop 1 · pages", from: desktopB, to: desktopA, timeline: fork(desktopB, desktopA, order: "wave"), press: controller)],
    [Scene(title: "Desktop 1 → Folder · open", from: desktopA, to: folder, timeline: fork(desktopA, folder, order: "rsnake"), press: 3)],
    [Scene(title: "Folder → Desktop 1 · back", from: folder, to: desktopA, timeline: fork(folder, desktopA, order: "snake"), press: controller)],
])

// MARK: the structure map

/// A finished page, drawn at `scale` with its bottom-left corner at `origin`.
func still(_ title: String, _ page: Page, at origin: CGPoint, scale: CGFloat) {
    NSGraphicsContext.saveGraphicsState()
    let transform = NSAffineTransform()
    transform.translateX(by: origin.x, yBy: origin.y)
    transform.scale(by: scale)
    transform.concat()
    drawDeck(title, from: page, to: page, timeline: Timeline(), press: -1, t: 1e9, origin: .zero)
    NSGraphicsContext.restoreGraphicsState()
}

func arrow(from a: CGPoint, to b: CGPoint, _ label: String, labelAt: CGPoint, both: Bool = false) {
    let line = NSBezierPath(); line.move(to: a); line.line(to: b); line.lineWidth = 2.5
    hex(0xffffff, 0.75).setStroke(); line.stroke()
    func head(at tip: CGPoint, from tail: CGPoint) {
        let angle = atan2(tip.y - tail.y, tip.x - tail.x)
        let p = NSBezierPath()
        p.move(to: tip)
        p.line(to: CGPoint(x: tip.x - 11 * cos(angle - 0.45), y: tip.y - 11 * sin(angle - 0.45)))
        p.line(to: CGPoint(x: tip.x - 11 * cos(angle + 0.45), y: tip.y - 11 * sin(angle + 0.45)))
        p.close(); hex(0xffffff, 0.75).setFill(); p.fill()
    }
    head(at: b, from: a)
    if both { head(at: a, from: b) }
    text(label, 13, hex(0xffffff, 0.85), at: labelAt, weight: .medium)
}

/// Outlines the three layers on a deck drawn by `still` at `origin` and `scale`.
func layers(at origin: CGPoint, scale: CGFloat) {
    func outline(_ p: Int, _ colour: NSColor) {
        let r = keyRect(p, origin: .zero)
        let rect = CGRect(x: origin.x + r.minX * scale, y: origin.y + r.minY * scale, width: r.width * scale, height: r.height * scale)
        let path = NSBezierPath(roundedRect: rect.insetBy(dx: -2, dy: -2), xRadius: 8, yRadius: 8)
        path.lineWidth = 3; colour.setStroke(); path.stroke()
    }
    for p in 0..<18 where !side.contains(p) && p != controller { outline(p, hex(0x60a5fa)) }
    outline(controller, hex(0xfb923c))
    let x = origin.x + (PAD + 5 * (K + GAP) + 5) * scale
    let sideRect = CGRect(x: x, y: origin.y + PAD * scale, width: K * 0.9 * scale, height: (3 * K + 2 * GAP) * scale)
    let path = NSBezierPath(roundedRect: sideRect.insetBy(dx: -2, dy: -2), xRadius: 8, yRadius: 8)
    path.lineWidth = 3; hex(0x4ade80).setStroke(); path.stroke()
}

do {
    let s: CGFloat = 0.62
    let w = deckW * s, h = deckH * s, gap: CGFloat = 120
    let size = CGSize(width: 2 * w + gap + 60, height: 2 * h + 150)
    let rep = canvas(size) {
        hex(0x18181b).setFill(); CGRect(origin: .zero, size: size).fill()
        let top = size.height - h - 20, bottom: CGFloat = 70
        let left: CGFloat = 30, right = left + w + gap
        still("Desktop 1", desktopA, at: CGPoint(x: left, y: top), scale: s)
        still("Desktop 2", desktopB, at: CGPoint(x: right, y: top), scale: s)
        still("Folder (in a key of Desktop 1)", folder, at: CGPoint(x: left, y: bottom), scale: s)
        let grey = desktopA.mapValues { tile -> Tile in
            switch tile {
            case .number(let n, _): return .number(n, -1)
            case .folder: return .folder(-1)
            default: return tile
            }
        }
        still("Layers", grey, at: CGPoint(x: right, y: bottom), scale: s)
        layers(at: CGPoint(x: right, y: bottom), scale: s)
        let midY = top + (deckH - HEADER) * s / 2
        arrow(from: CGPoint(x: left + w + 12, y: midY), to: CGPoint(x: right - 12, y: midY), "pages",
              labelAt: CGPoint(x: left + w + gap / 2 - 18, y: midY + 8), both: true)
        let fx = left + keyRect(3, origin: .zero).midX * s
        arrow(from: CGPoint(x: fx, y: top - 6), to: CGPoint(x: fx, y: bottom + h + 4), "open", labelAt: CGPoint(x: fx + 8, y: top - 48))
        let bx = left + keyRect(controller, origin: .zero).midX * s
        arrow(from: CGPoint(x: bx, y: bottom + h + 4), to: CGPoint(x: bx, y: top - 6), "back", labelAt: CGPoint(x: bx + 8, y: top - 48))
        let legend = [("controller: pages / back", hex(0xfb923c)), ("side screen", hex(0x4ade80)), ("content", hex(0x60a5fa))]
        let width = { (name: String) in
            17 + NSAttributedString(string: name, attributes: [.font: NSFont.systemFont(ofSize: 12, weight: .semibold)]).size().width + 16
        }
        var x = right + w - legend.map { width($0.0) }.reduce(0, +) + 16 // right-aligned under the Layers deck
        for (name, colour) in legend {
            colour.setFill(); NSBezierPath(roundedRect: CGRect(x: x, y: 33, width: 12, height: 12), xRadius: 3, yRadius: 3).fill()
            text(name, 12, hex(0xffffff, 0.8), at: CGPoint(x: x + 17, y: 30))
            x += width(name)
        }
    }
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "\(out)/structure-\(VERSION).png"))
    print("structure-\(VERSION).png")
}
