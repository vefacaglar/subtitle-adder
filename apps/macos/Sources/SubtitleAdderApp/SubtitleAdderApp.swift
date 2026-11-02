import AppKit
import EngineKit
import Foundation
import LibraryKit
import SwiftUI

@main
struct SubtitleAdderApp: App {
    @State private var catalog: ModelCatalog
    @State private var vm: SubtitleViewModel
    @State private var explorer: FileExplorerModel
    @State private var platform: VideoPlatformModel
    @State private var language: AppLanguage
    /// Finder hands several files over one by one; they are opened together.
    @State private var externalOpen: ExternalOpenCoalescer
    @NSApplicationDelegateAdaptor(ExternalOpenDelegate.self) private var openDelegate

    init() {
        // If the engine exits early, writing to stdin shouldn't kill the app with SIGPIPE.
        signal(SIGPIPE, SIG_IGN)
        // Come to the front and show in the Dock even when launched without a bundle via `swift run`.
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate()
        AppearanceController.shared.start()
        // Must exist before anything builds a localized string.
        _language = State(initialValue: AppLanguage())
        // Nothing runs yet, so the engine's scratch folder from earlier sessions can go.
        WorkingDirectory.cleanUp()
        let catalog = ModelCatalog()
        // The real providers file and keychain; the view model migrates the older single LAN server settings once.
        let model = SubtitleViewModel(catalog: catalog, providerStore: TranslationProviderStore())
        _catalog = State(initialValue: catalog)
        _vm = State(initialValue: model)
        let opener = ExternalOpenCoalescer { model.openExternalFiles($0) }
        ExternalOpenDelegate.coalescer = opener
        _externalOpen = State(initialValue: opener)
        let explorer = FileExplorerModel()
        _explorer = State(initialValue: explorer)
        // The real profiles file and keychain; the platform shares the explorer's media index.
        let platform = VideoPlatformModel(store: ProfileStore(), index: explorer.index)
        _platform = State(initialValue: platform)
        // A subtitle written next to a video gets its badge in the sidebar.
        model.onSubtitleSaved = { url in Task { await explorer.subtitleSaved(at: url) } }
        // On quit, clear the scratch folder again unless a job is still running (the next launch gets it then).
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { _ in
            MainActor.assumeIsolated {
                // Where the video in the player was, so it resumes there.
                platform.prepareForQuit()
                if !model.isWorking { WorkingDirectory.cleanUp() }
            }
        }
    }

    var body: some Scene {
        WindowGroup("Subtitle Adder") {
            ContentView(vm: vm, catalog: catalog, explorer: explorer, platform: platform)
                .frame(minWidth: ContentView.minimumWindowWidth, minHeight: 600)
                // Opening with "Open With" from Finder / dropping on the Dock: don't open a new window, hand it to the open one.
                .handlesExternalEvents(preferring: ["*"], allowing: ["*"])
                // SwiftUI takes the first file of an open request here and passes the rest to `ExternalOpenDelegate`.
                .onOpenURL { externalOpen.receive($0) }
                .environment(\.locale, language.effective.locale)
        }
        // Room for the sidebar next to the editor and the toolbar.
        .defaultSize(width: 1180, height: 720)
        .commands {
            LayoutCommands(vm: vm)
            FileExplorerCommands(explorer: explorer)
        }
        Settings {
            SettingsView(vm: vm, catalog: catalog, language: language)
                .environment(\.locale, language.effective.locale)
        }
    }
}

extension FocusedValues {
    /// The view model of the focused main window; `nil` while another window (Settings) is key.
    @Entry var subtitleVM: SubtitleViewModel?
    /// Opens the video picker of the focused main window; `nil` while none can pick (or Settings is key). Each window
    /// publishes its own action, so ⌘O only ever reaches the key window.
    @Entry var chooseVideo: ChooseVideoAction?
}

/// A window's "open the video picker" action (see `FocusedValues.chooseVideo`).
struct ChooseVideoAction {
    let perform: @MainActor () -> Void
}

/// Menu commands for open video (⌘O), generate (⌘↩) and stop (⌘.), so they keep working while theater mode hides the toolbar. They act
/// on the focused main window's view model, so they do nothing while Settings is key (⌘↩ there must not restart a job).
/// Full screen (⌃⌘F) is the system's own View menu item.
private struct LayoutCommands: Commands {
    @Bindable var vm: SubtitleViewModel
    @FocusedValue(\.subtitleVM) private var focused
    @FocusedValue(\.chooseVideo) private var chooseVideo

    var body: some Commands {
        CommandGroup(after: .newItem) {
            Button(L("Video aç…"), systemImage: "film") { chooseVideo?.perform() }
                .keyboardShortcut("o", modifiers: .command)
                .disabled(chooseVideo == nil)
            Button(L("Altyazı üret"), systemImage: "captions.bubble") { focused?.generate() }
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(focused.map { $0.video == nil || $0.isWorking || $0.workspace != .editor } ?? true)
            Button(L("Durdur"), systemImage: "stop.fill") { focused?.cancel() }
                .keyboardShortcut(".", modifiers: .command)
                .disabled(focused.map { !$0.isWorking } ?? true)
        }
        CommandGroup(after: .toolbar) {
            Toggle(L("Tiyatro modu"), isOn: $vm.isTheaterMode)
            Toggle(L("Altyazıları sağa / alta al"), isOn: $vm.sideBySideLayout)
                .disabled(vm.hideSubtitlePanel)
            // Only while a player is on screen; the E key does the same.
            Button(vm.hideSubtitlePanel ? L("Altyazı panelini göster") : L("Altyazı panelini gizle")) {
                focused?.toggleSubtitlePanel()
            }
            .disabled(focused.map { !$0.isPlayerShown } ?? true)
        }
    }
}
