// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "RiWorkRemote",
    platforms: [.macOS(.v14), .iOS(.v18)],
    products: [.library(name: "RiWorkCore", targets: ["RiWorkCore"]),
               .executable(name: "riwork-ios-smoke", targets: ["RiWorkSmoke"])],
    targets: [
        .target(name: "RiWorkCore", path: "Core"),
        .executableTarget(name: "RiWorkSmoke", dependencies: ["RiWorkCore"], path: "Smoke"),
        .testTarget(name: "RiWorkCoreTests", dependencies: ["RiWorkCore"], path: "Tests", resources: [.copy("Fixtures")])
    ]
)
