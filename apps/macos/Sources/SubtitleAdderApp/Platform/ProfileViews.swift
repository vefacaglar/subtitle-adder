import AppKit
import SwiftUI

/// Username, password, "Girişi hatırla" and the two buttons; the profile popover and the hub's sign-in card share it.
/// The work (PBKDF2) happens off the main actor in the model, so the form only shows a spinner meanwhile.
struct ProfileAuthForm: View {
    let model: VideoPlatformModel
    /// "Yeni profil oluştur" in the popover, "Profil oluştur" on the card.
    let registerTitle: String
    var onSuccess: () -> Void = {}

    @State private var username = ""
    @State private var password = ""
    @FocusState private var focus: Field?

    private enum Field { case username, password }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            if !model.registeredUsernames.isEmpty {
                HStack(spacing: 6) {
                    ForEach(model.registeredUsernames.prefix(4), id: \.self) { name in
                        Button(name) {
                            username = name
                            focus = .password
                        }
                        .buttonStyle(PillButtonStyle(isSelected: false, compact: true))
                        .lineLimit(1)
                    }
                }
            }
            TextField(L("Kullanıcı adı"), text: $username)
                .textFieldStyle(.roundedBorder)
                .textContentType(.username)
                .focused($focus, equals: .username)
                .onSubmit { focus = .password }
                .disabled(model.isAuthenticating)
            SecureField(L("Şifre"), text: $password)
                .textFieldStyle(.roundedBorder)
                .textContentType(.password)
                .focused($focus, equals: .password)
                .onSubmit(login)
                .disabled(model.isAuthenticating)
            Toggle(L("Girişi hatırla"), isOn: $model.rememberMe)
                .disabled(model.isAuthenticating)
            if let error = model.authError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                Button(L("Giriş yap"), action: login)
                    .keyboardShortcut(.defaultAction)
                Button(registerTitle, action: register)
                if model.isAuthenticating {
                    ProgressView().controlSize(.small)
                }
            }
            .disabled(model.isAuthenticating)
        }
        .onAppear {
            model.clearAuthFailure()
            fillLastUsername()
        }
        .onChange(of: model.lastUsername) { fillLastUsername() }
        .onDisappear { model.clearAuthFailure() }
    }

    private func fillLastUsername() {
        guard username.isEmpty, let last = model.lastUsername else { return }
        username = last
    }

    private func login() {
        let name = username, secret = password
        Task {
            if await model.login(username: name, password: secret) { finish() }
        }
    }

    private func register() {
        let name = username, secret = password
        Task {
            if await model.register(username: name, password: secret) { finish() }
        }
    }

    private func finish() {
        password = ""
        onSuccess()
    }
}

/// The hub's sign-in card, in place of the videos while logged out.
struct ProfileAuthCard: View {
    let model: VideoPlatformModel

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "person.crop.circle.badge.checkmark")
                .font(.system(size: 40, weight: .light))
                .foregroundStyle(.tint)
            Text(L("Yerel Video Platformu"))
                .font(.title2.weight(.semibold))
            Text(L("İzleme geçmişi, görüntülenme sayısı, favoriler, yıldız puanları ve özel listeleriniz yerel profilinize kaydedilir. Devam etmek için giriş yapın veya yeni profil oluşturun."))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            ProfileAuthForm(model: model, registerTitle: L("Profil oluştur"))
                .frame(maxWidth: 320)
        }
        .padding(24)
        .frame(maxWidth: 440)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(.separator))
    }
}

/// The toolbar's profile button and the popover it opens.
struct ProfileToolbarButton: View {
    let model: VideoPlatformModel
    let openPlatform: () -> Void
    @State private var isPresented = false

    var body: some View {
        Button {
            isPresented.toggle()
        } label: {
            Label(model.profileTitle, systemImage: "person.crop.circle")
                .labelStyle(.titleAndIcon)
        }
        .help(model.isLoggedIn ? L("Profil") : L("İzleme geçmişi, beğeni ve puanlama için giriş yapın"))
        .popover(isPresented: $isPresented, arrowEdge: .bottom) {
            ProfilePopover(model: model, openPlatform: openPlatform, dismiss: { isPresented = false })
        }
    }
}

struct ProfilePopover: View {
    let model: VideoPlatformModel
    let openPlatform: () -> Void
    let dismiss: () -> Void

    var body: some View {
        Group {
            if let name = model.username {
                loggedIn(name)
            } else {
                loggedOut
            }
        }
        .padding(16)
        .frame(width: 300)
    }

    private var loggedOut: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Label(L("Profil / Giriş"), systemImage: "person.crop.circle")
                    .font(.headline)
                Text(L("İzleme geçmişi, görüntülenme sayısı, beğeni ve yıldız puanları profilinize özel kaydedilir."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ProfileAuthForm(model: model, registerTitle: L("Yeni profil oluştur"), onSuccess: dismiss)
        }
    }

    private func loggedIn(_ name: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "person.crop.circle.fill")
                    .font(.system(size: 34))
                    .foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.headline).lineLimit(1)
                    Text(model.summaryText).font(.caption).foregroundStyle(.secondary)
                }
            }
            Divider()
            Button {
                dismiss()
                openPlatform()
            } label: {
                Label(L("Video Platformu"), systemImage: "play.rectangle.on.rectangle")
            }
            Button {
                dismiss()
                model.logout()
            } label: {
                Label(L("Çıkış yap"), systemImage: "rectangle.portrait.and.arrow.right")
            }
            Divider()
            DeleteProfileSection(model: model, username: name, onDeleted: dismiss)
        }
        .buttonStyle(.borderless)
    }
}

/// "Profili sil…": asks for the profile's password, then once more whether to go through with it.
private struct DeleteProfileSection: View {
    let model: VideoPlatformModel
    let username: String
    let onDeleted: () -> Void

    private enum Step { case closed, password, confirm }

    @State private var step = Step.closed
    @State private var password = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch step {
            case .closed:
                Button(role: .destructive) {
                    model.clearAuthFailure()
                    step = .password
                } label: {
                    Label(L("Profili sil…"), systemImage: "trash")
                }
            case .password:
                Text(L("“%@” profilini silmek için şifresini girin.", username))
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                SecureField(L("Şifre"), text: $password)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(next)
                errorLabel
                HStack {
                    Button(L("Vazgeç"), action: close)
                    Spacer()
                    Button(L("Devam"), role: .destructive, action: next)
                        .disabled(password.isEmpty)
                }
            case .confirm:
                Text(L("“%@” profili ve içindeki izleme geçmişi, favoriler, puanlar ve listeler kalıcı olarak silinsin mi? Bu işlem geri alınamaz.", username))
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                errorLabel
                HStack {
                    Button(L("Vazgeç"), action: close)
                    Spacer()
                    if model.isAuthenticating { ProgressView().controlSize(.small) }
                    Button(L("Profili sil"), role: .destructive, action: delete)
                        .disabled(model.isAuthenticating)
                }
            }
        }
        .onDisappear { model.clearAuthFailure() }
    }

    @ViewBuilder private var errorLabel: some View {
        if let error = model.authError {
            Label(error, systemImage: "exclamationmark.triangle.fill")
                .font(.caption)
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func next() {
        guard !password.isEmpty else { return }
        model.clearAuthFailure()
        step = .confirm
    }

    private func close() {
        password = ""
        model.clearAuthFailure()
        step = .closed
    }

    private func delete() {
        let secret = password
        Task {
            if await model.deleteProfile(username: username, password: secret) {
                password = ""
                onDeleted()
            } else {
                // A wrong password: back to asking for it.
                step = .password
            }
        }
    }
}

/// A capsule button for the filters, the lists and the profile names. Accent-filled when selected.
struct PillButtonStyle: ButtonStyle {
    let isSelected: Bool
    var compact = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(compact ? .caption : .callout)
            .fontWeight(isSelected ? .semibold : .regular)
            .padding(.horizontal, compact ? 9 : 12)
            .padding(.vertical, compact ? 3 : 5)
            .foregroundStyle(isSelected ? Color.white : Color.primary)
            .background(isSelected ? Color.accentColor : Color.primary.opacity(0.08), in: Capsule())
            .opacity(configuration.isPressed ? 0.7 : 1)
            .contentShape(Capsule())
    }
}
