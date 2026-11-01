import AppKit
import Observation
import SwiftUI

/// The window-level things SwiftUI can't reach: native full screen (kept in step with the window itself, so the
/// system's ⌃⌘F, the green button and Esc all agree with the toggle in the player) and the Esc key.
@MainActor
@Observable
final class WindowChrome {
    private(set) var isFullScreen = false
    /// Esc with no text being edited and the window not full screen: returns whether it did something (leaving theater mode).
    @ObservationIgnored var onEscape: (@MainActor () -> Bool)?
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
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            // Only plain values cross into the main actor; the event itself stays here.
            let keyCode = event.keyCode
            let plain = event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty
            let target = event.window.map(ObjectIdentifier.init)
            let consumed = MainActor.assumeIsolated {
                self?.handleKey(code: keyCode, hasNoModifiers: plain, target: target) ?? false
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

    /// Esc leaves full screen first, then theater mode; while text is being edited it belongs to the text field.
    /// Returns whether the key was used.
    private func handleKey(code: UInt16, hasNoModifiers: Bool, target: ObjectIdentifier?) -> Bool {
        guard code == 53, hasNoModifiers, let window, target == ObjectIdentifier(window),
              !(window.firstResponder is NSText)
        else { return false }
        if window.styleMask.contains(.fullScreen) {
            window.toggleFullScreen(nil)
            return true
        }
        return onEscape?() == true
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
