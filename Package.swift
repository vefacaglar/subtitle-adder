// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "SubtitleAdder",
    platforms: [.macOS(.v15)],
    products: [
        .library(name: "SubtitleCore", targets: ["SubtitleCore"]),
        .executable(name: "subtitle-cli", targets: ["subtitle-cli"]),
        .executable(name: "SubtitleAdderApp", targets: ["SubtitleAdderApp"]),
    ],
    dependencies: [
        .package(url: "https://github.com/argmaxinc/WhisperKit.git", from: "0.9.0"),
    ],
    targets: [
        // Platformdan bağımsız çekirdek: modeller, cümle bölme, altyazı yazıcı, pipeline.
        .target(name: "SubtitleCore"),
        // STT backend'i (WhisperKit, on-device CoreML).
        .target(
            name: "WhisperTranscriber",
            dependencies: ["SubtitleCore", .product(name: "WhisperKit", package: "WhisperKit")]
        ),
        .executableTarget(name: "subtitle-cli", dependencies: ["SubtitleCore", "WhisperTranscriber"]),
        .executableTarget(name: "SubtitleAdderApp", dependencies: ["SubtitleCore", "WhisperTranscriber"]),
        .testTarget(name: "SubtitleCoreTests", dependencies: ["SubtitleCore"]),
    ]
)
