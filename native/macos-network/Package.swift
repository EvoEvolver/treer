// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "TreerNetwork",
    platforms: [.macOS(.v13)],
    products: [.library(name: "TreerNetworkCore", targets: ["TreerNetworkCore"])],
    targets: [
        .target(name: "TreerNetworkCore"),
        .testTarget(name: "TreerNetworkCoreTests", dependencies: ["TreerNetworkCore"]),
    ]
)
