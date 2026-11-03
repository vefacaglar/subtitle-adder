import AppKit
import Observation
import SwiftUI

/// The window-level things SwiftUI can't reach: native full screen (kept in step with the window itself, so the
/// system's ⌃⌘F, the green button and Esc all agree with the toggle in the player), the Esc key and the player keys
/// (Space, arrows, M, F, E), which are read from the window's key events so they work wherever the focus is.
@MainActor
@Observable
final class WindowChrome {
    private(set) var isFullScreen = false
    /// Esc with no text being edited and the window not full screen: returns whether it did something (leaving theater mode).
    @ObservationIgnored var onEscape: (@MainActor () -> Bool)?
    /// A player key (and whether it is an auto-repeat): returns whether it was used. When it isn't (no player on
    /// screen), the key goes on to the focused view as usual.
    @ObservationIgnored var onShortcut: (@MainActor (PlayerShortcut, Bool) -> Bool)?
    /// Whether the user has hidden the subtitle panel; without a list on screen the up and down arrows change the volume
    /// even when a list holds the focus.
    @ObservationIgnored var isPanelHidden: @MainActor () -> Bool = { false }
    /// The window is closing.
    @ObservationIgnored var onClose: (@MainActor () -> Void)?
    /// The window became the key window.
    @ObservationIgnored var onBecomeKey: (@MainActor () -> Void)?
    @ObservationIgnored private weak var window: NSWindow?
    @ObservationIgnored private var observers: [NSObjectProtocol] = []
    @ObservationIgnored private var keyMonitor: Any?

    func toggleFullScreen() {
        window?.toggleFullScreen(nil)
    }

    /// Follows `window`; called again whenever the hosting view moves to another window (or none).
    func attach(_ window: NSWindow?) {
        guard window !== self.window else { return }
        detach()
        self.window = window
        guard let window else {
            isFullScreen = false
            return
        }
        isFullScreen = window.styleMask.contains(.fullScreen)
        let center = NotificationCenter.default
        for (name, value) in [(NSWindow.didEnterFullScreenNotification, true), (NSWindow.didExitFullScreenNotification, false)] {
            observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.isFullScreen = value }
            })
        }
        observers.append(center.addObserver(forName: NSWindow.willCloseNotification, object: window, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.onClose?() }
        })
        observers.append(center.addObserver(forName: NSWindow.didBecomeKeyNotification, object: window, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.onBecomeKey?() }
        })
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            // Only plain values cross into the main actor; the event itself stays here.
            let keyCode = event.keyCode
            // Caps Lock, numeric pad and function flags don't change what a key means, so only the chord keys count.
            let modifiers = event.modifierFlags.intersection([.shift, .control, .option, .command])
            let character = event.charactersIgnoringModifiers
            let isRepeat = event.isARepeat
            let target = event.window.map(ObjectIdentifier.init)
            let consumed = MainActor.assumeIsolated {
                self?.handleKey(
                    code: keyCode, character: character, modifiers: modifiers, isRepeat: isRepeat, target: target
                ) ?? false
            }
            return consumed ? nil : event
        }
    }

    private func detach() {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers = []
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
    }

    /// Esc leaves full screen first, then theater mode; the player keys act on the player. While text is being edited
    /// every key belongs to the text field. Keys aimed at another window (Settings, a sheet) are not ours. Returns
    /// whether the key was used.
    private func handleKey(
        code: UInt16, character: String?, modifiers: NSEvent.ModifierFlags, isRepeat: Bool, target: ObjectIdentifier?
    ) -> Bool {
        guard let window, target == ObjectIdentifier(window), !(window.firstResponder is NSText) else { return false }
        if code == 53 {
            guard modifiers.isEmpty else { return false }
            if window.styleMask.contains(.fullScreen) {
                window.toggleFullScreen(nil)
                return true
            }
            return onEscape?() == true
        }
        guard let onShortcut, let shortcut = PlayerShortcut.resolve(
            keyCode: code, character: character, modifiers: modifiers, focus: focus(in: window)
        ) else { return false }
        return onShortcut(shortcut, isRepeat)
    }

    /// Where the keyboard focus is, from the window's first responder.
    private func focus(in window: NSWindow) -> PlayerShortcut.Focus {
        let responder = window.firstResponder
        var focus = PlayerShortcut.Focus()
        focus.isEditingText = responder is NSText
        focus.isOnButton = responder is NSButton || responder is NSSegmentedControl
        var view = responder as? NSView
        while let current = view {
            if current is NSTableView {
                focus.isInList = true
                break
            }
            view = current.superview
        }
        focus.isFullScreen = window.styleMask.contains(.fullScreen)
        focus.panelHidden = isPanelHidden()
        return focus
    }
}

/// Hands the window that hosts the view to `onChange`.
struct WindowReader: NSViewRepresentable {
    let onChange: @MainActor (NSWindow?) -> Void

    func makeNSView(context: Context) -> NSView {
        let view = WindowTrackingView()
        view.onChange = onChange
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        (view as? WindowTrackingView)?.onChange = onChange
    }

    private final class WindowTrackingView: NSView {
        var onChange: (@MainActor (NSWindow?) -> Void)?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            let window = window
            let onChange = onChange
            // The window may not be fully set up yet; reporting on the next turn is also outside the view update.
            Task { @MainActor in onChange?(window) }
        }
    }
}
