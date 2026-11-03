import SwiftUI

/// The question for every way of opening a file (sidebar, platform card, picker, drop, Finder) that would stop a running
/// job or drop edited lines. See `SubtitleViewModel.requestOpen`.
private struct OpenConfirmation: ViewModifier {
    let vm: SubtitleViewModel

    func body(content: Content) -> some View {
        content.confirmationDialog(
            L("Başka bir dosya açılsın mı?"),
            isPresented: Binding(get: { vm.pendingOpen != nil }, set: { if !$0 { vm.cancelPendingOpen() } }),
            titleVisibility: .visible,
            presenting: vm.pendingOpen
        ) { pending in
            // The presented request itself, not `vm.pendingOpen`: the dialog closing may clear that first.
            Button(L("Devam")) {
                vm.pendingOpen = nil
                pending.perform()
            }
            Button(L("Vazgeç"), role: .cancel) { vm.cancelPendingOpen() }
        } message: { pending in
            Text(pending.message)
        }
    }
}

extension View {
    func confirmsOpening(in vm: SubtitleViewModel) -> some View {
        modifier(OpenConfirmation(vm: vm))
    }
}
