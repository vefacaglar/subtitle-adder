import Foundation

/// Everything the profile layer can fail with. The module has no user-facing text: the app maps each case to a
/// localized message.
public enum ProfileError: Error, Equatable, Sendable {
    case emptyUsername
    case emptyPassword
    /// Login or delete without a username or without a password.
    case missingCredentials
    case usernameTaken
    /// Unknown user or wrong password (deliberately indistinguishable).
    case invalidCredentials
    /// The password is right but the profile's vault could not be decrypted or parsed. The stored ciphertext is left
    /// untouched, so a later build may still be able to read it.
    case vaultUnreadable
    case emptyListName
    /// Another list of the profile already has this name (compared ignoring case and diacritics).
    case duplicateListName
    /// The cross-process lock on `profiles.json.lock` could not be taken in time; nothing was written.
    case lockUnavailable
    /// `profiles.json` could not be written; `errno` is the failing system call's code.
    case writeFailed(errno: Int32)
    /// `profiles.json` is unreadable or corrupt and a copy of it could not be set aside, so it was not overwritten.
    case corruptFileNotPreserved
    /// `profiles.json` was written by a newer version of the format and is left alone.
    case unsupportedFileVersion(Int)
    /// `profiles.json` exists but could not be read just now (permissions, an I/O error), so this store does not know
    /// what is in it and refuses to write: replacing it with its own, possibly empty, view would lose the profiles.
    /// Nothing was changed on disk; every later call reads the file again, so the same call works once it is readable.
    case fileUnreadable
    /// Key derivation or encryption failed (should not happen).
    case cryptoFailure
}
