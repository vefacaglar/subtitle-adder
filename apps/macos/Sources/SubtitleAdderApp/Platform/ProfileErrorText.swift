import Foundation
import LibraryKit

extension ProfileError {
    /// The message shown to the user, in the current UI language. `LibraryKit` has no text of its own.
    var message: String {
        switch self {
        case .emptyUsername: L("Lütfen bir kullanıcı adı girin.")
        case .emptyPassword: L("Lütfen bir şifre belirleyin.")
        case .missingCredentials: L("Kullanıcı adı ve şifre gereklidir.")
        case .usernameTaken: L("Bu kullanıcı adı zaten kayıtlı.")
        case .invalidCredentials: L("Kullanıcı adı veya şifre hatalı.")
        case .vaultUnreadable:
            L("Şifre doğru, ancak profilin şifreli verisi okunamadı. Dosya değiştirilmedi; başka bir sürümle açmayı deneyin.")
        case .emptyListName: L("Lütfen bir liste adı girin.")
        case .duplicateListName: L("Bu adda bir liste zaten var.")
        case .lockUnavailable: L("Profil dosyası şu an başka bir işlem tarafından kullanılıyor. Biraz sonra tekrar deneyin.")
        case .writeFailed(let code): L("Profil dosyası yazılamadı (hata %lld).", Int(code))
        case .unsupportedFileVersion(let version):
            L("Profil dosyası uygulamanın daha yeni veya başka bir sürümü tarafından yazılmış (sürüm %lld); burada açılamaz ve değiştirilmeyecek.", version)
        case .corruptFileNotPreserved:
            L("Profil dosyası bozuk ve yedeği alınamadığı için üzerine yazılmadı.")
        case .fileUnreadable: L("Profil dosyası okunamıyor; değişiklikler kaydedilmedi.")
        case .cryptoFailure: L("Şifreleme işlemi başarısız oldu.")
        case .listNotFound: L("Bu liste artık yok.")
        }
    }
}
