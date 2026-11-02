import AppKit
import Testing
@testable import SubtitleAdderApp

/// The player keys through a real window: key events go through `NSApplication.sendEvent`, so the window's local
/// monitor and its first-responder checks run exactly as in the app.
@MainActor
@Suite struct WindowKeysTests {
    /// Swallows keys nobody handled, so a test doesn't beep.
    private final class QuietWindow: NSWindow {
        override func keyDown(with event: NSEvent) {}
        // A window that was never shown still takes keys, as the key window.
        override var isKeyWindow: Bool { true }
        override var canBecomeKey: Bool { true }
    }

    /// A table view that takes the focus without rows or a data source.
    private final class FocusableTable: NSTableView {
        override var acceptsFirstResponder: Bool { true }
    }

    private final class Recorder {
        var received: [(PlayerShortcut, Bool)] = []
        var shortcuts: [PlayerShortcut] { received.map(\.0) }
    }

    private struct Fixture {
        let window: QuietWindow
        let chrome: WindowChrome
        let recorder: Recorder
        let content: NSView
    }

    private func makeFixture(panelHidden: Bool = false, uses: Bool = true) -> Fixture {
        let window = QuietWindow(
            contentRect: NSRect(x: 0, y: 0, width: 400, height: 300), styleMask: [.titled], backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        let content = NSView(frame: window.contentRect(forFrameRect: window.frame))
        window.contentView = content
        let chrome = WindowChrome()
        let recorder = Recorder()
        chrome.onShortcut = { shortcut, isRepeat in
            recorder.received.append((shortcut, isRepeat))
            return uses
        }
        chrome.isPanelHidden = { panelHidden }
        chrome.attach(window)
        return Fixture(window: window, chrome: chrome, recorder: recorder, content: content)
    }

    private func press(
        _ keyCode: UInt16, _ characters: String = "", modifiers: NSEvent.ModifierFlags = [], isRepeat: Bool = false, in window: NSWindow
    ) {
        let event = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil, characters: characters, charactersIgnoringModifiers: characters,
            isARepeat: isRepeat, keyCode: keyCode
        )!
        NSApplication.shared.sendEvent(event)
    }

    @Test func playerKeysReachTheHandlerFromAPlainView() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        press(49, " ", in: fixture.window)
        press(124, "\u{F703}", in: fixture.window)
        press(123, "\u{F702}", modifiers: .shift, in: fixture.window)
        press(46, "m", in: fixture.window)
        press(3, "f", in: fixture.window)
        press(14, "e", in: fixture.window)
        press(126, "\u{F700}", in: fixture.window)
        #expect(fixture.recorder.shortcuts == [
            .togglePlayback, .seek(5), .seek(-10), .toggleMute, .toggleFullScreen, .toggleSubtitlePanel, .adjustVolume(0.05),
        ])
    }

    @Test func autoRepeatIsPassedOn() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        press(124, "\u{F703}", isRepeat: true, in: fixture.window)
        #expect(fixture.recorder.received.count == 1 && fixture.recorder.received[0].1)
    }

    @Test func nothingFiresWhileATextFieldIsBeingEdited() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 200, height: 24))
        fixture.content.addSubview(field)
        #expect(fixture.window.makeFirstResponder(field))
        // The field editor, an NSTextView, is the first responder now.
        #expect(fixture.window.firstResponder is NSText)
        for (code, characters) in [(49, " "), (124, "\u{F703}"), (125, "\u{F701}"), (46, "m"), (3, "f"), (14, "e")] as [(UInt16, String)] {
            press(code, characters, in: fixture.window)
        }
        #expect(fixture.recorder.received.isEmpty)
    }

    @Test func chordsWithCommandControlOrOptionAreLeftAlone() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        press(49, " ", modifiers: .command, in: fixture.window)
        press(124, "\u{F703}", modifiers: .control, in: fixture.window)
        press(46, "m", modifiers: .option, in: fixture.window)
        press(3, "f", modifiers: [.command, .control], in: fixture.window)
        #expect(fixture.recorder.received.isEmpty)
    }

    @Test func spaceStaysWithAFocusedButton() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        let button = NSButton(frame: NSRect(x: 0, y: 0, width: 100, height: 24))
        fixture.content.addSubview(button)
        #expect(fixture.window.makeFirstResponder(button))
        press(49, " ", in: fixture.window)
        #expect(fixture.recorder.received.isEmpty)
        // The arrows still reach the player.
        press(124, "\u{F703}", in: fixture.window)
        #expect(fixture.recorder.shortcuts == [.seek(5)])
    }

    @Test func aFocusedListKeepsUpAndDownUnlessThePanelIsHidden() {
        for hidden in [false, true] {
            let fixture = makeFixture(panelHidden: hidden)
            defer { fixture.chrome.attach(nil) }
            let table = FocusableTable(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
            let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
            scroll.documentView = table
            fixture.content.addSubview(scroll)
            #expect(fixture.window.makeFirstResponder(table))
            #expect(fixture.window.firstResponder === table, "first responder: \(String(describing: fixture.window.firstResponder))")
            press(126, "\u{F700}", in: fixture.window)
            press(125, "\u{F701}", in: fixture.window)
            press(49, " ", in: fixture.window)
            press(123, "\u{F702}", in: fixture.window)
            if hidden {
                #expect(fixture.recorder.shortcuts == [.adjustVolume(0.05), .adjustVolume(-0.05), .togglePlayback, .seek(-5)])
            } else {
                #expect(fixture.recorder.shortcuts == [.togglePlayback, .seek(-5)])
            }
        }
    }

    @Test func keysForAnotherWindowAreNotOurs() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        let other = QuietWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 100), styleMask: [.titled], backing: .buffered, defer: false
        )
        other.isReleasedWhenClosed = false
        press(49, " ", in: other)
        #expect(fixture.recorder.received.isEmpty)
    }

    @Test func escLeavesTheEscapeHandlerOutOfThePlayerKeys() {
        let fixture = makeFixture()
        defer { fixture.chrome.attach(nil) }
        var escapes = 0
        fixture.chrome.onEscape = {
            escapes += 1
            return true
        }
        press(53, "\u{1B}", in: fixture.window)
        press(53, "\u{1B}", modifiers: .shift, in: fixture.window)
        #expect(escapes == 1)
        #expect(fixture.recorder.received.isEmpty)
    }
}
