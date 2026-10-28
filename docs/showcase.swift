// Renders the README's showcase GIFs: a simulated 3x6 deck switching pages, timed with what was
// measured on a Maddog GK150K. It is a simulation, not a screen recording.
//
//   swift docs/showcase.swift docs
//
// Timing model (ms after the key press):
//   OpenDeck redraws the new page and the driver starts sending after ~130 (upstream: ~150);
//   each key image then takes ~30, and each refresh (STP), which shows what was sent, ~40.
//   Upstream also clears the whole screen at ~20, so the deck is black until its single refresh.
import AppKit
import ImageIO
import UniformTypeIdentifiers

let out = CommandLine.arguments.dropFirst().first ?? "docs"

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
    let done = 150.0 + 18 * 30 + 40
    var t = Timeline(black: (20, done))
    for p in 0..<18 { t.change[p] = done }
    return t
}

/// The fork: unchanged keys are skipped; controller and side go first; content in waves.
func fork(_ from: Page, _ to: Page, order: String, vanish: Bool = false) -> Timeline {
    var t = Timeline()
    var clock = 130.0
    let changed = (0..<18).filter { from[$0] != to[$0] }
    let chrome = changed.filter { side.contains($0) || $0 == controller }
    var content = changed.filter { !chrome.contains($0) }
    func wave(_ keys: [Int]) {
        guard !keys.isEmpty else { return }
        clock += Double(keys.count) * 30
        for p in keys { t.change[p] = clock }
        clock += 40
    }
    if vanish { // darken every content key first, then draw the folder's own keys
        let all = (0..<18).filter { !side.contains($0) && $0 != controller }
        let darkAt = clock + Double(all.count + chrome.count) * 30
        for p in all { t.dark[p] = (darkAt, darkAt) }
        for p in chrome { t.change[p] = darkAt }
        for p in all where to[p] == .empty { t.change[p] = darkAt }
        clock = darkAt + 40
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
        let top = NSColor(hue: hue, saturation: 0.38, brightness: 1, alpha: 1)
        let bottom = NSColor(hue: hue, saturation: 0.55, brightness: 0.93, alpha: 1)
        NSGradient(starting: top, ending: bottom)!.draw(in: shape, angle: -90)
        text("\(n)", 30, hex(0x1e1b4b, 0.8), at: CGPoint(x: rect.midX, y: rect.midY - 19), weight: .heavy, center: true)
    case .folder(let hue):
        NSColor(hue: hue, saturation: 0.5, brightness: 0.45, alpha: 1).setFill(); shape.fill()
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
    let url = URL(fileURLWithPath: "\(out)/\(name).gif")
    var frames: [CGImage] = []
    var delays: [Double] = []
    for scenes in steps {
        let end = scenes.map { $0.timeline.end }.max()!
        var t = -400.0
        while t <= end + 900 {
            let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width), pixelsHigh: Int(size.height),
                                       bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                       colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
            hex(0x18181b).setFill(); CGRect(origin: .zero, size: size).fill()
            for (i, s) in scenes.enumerated() {
                let y = size.height - CGFloat(i + 1) * (deckH + 12)
                drawDeck(s.title, from: s.from, to: s.to, timeline: s.timeline, press: s.press, t: t, origin: CGPoint(x: 12, y: y))
            }
            NSGraphicsContext.restoreGraphicsState()
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
    print("\(name).gif: \(frames.count) frames")
}

// MARK: the showcase

gif("before-after", [[
    Scene(title: "upstream 0.11.1", from: desktopA, to: desktopB, timeline: upstream(desktopA, desktopB), press: controller),
    Scene(title: "this fork · wave", from: desktopA, to: desktopB, timeline: fork(desktopA, desktopB, order: "wave"), press: controller),
]])
for order in ["classic", "wave", "snake"] {
    gif("page-\(order)", [[Scene(title: "page switch · \(order)", from: desktopA, to: desktopB,
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
