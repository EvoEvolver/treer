"""Build and verify the native backend without signing or installing it."""

import argparse
import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--developer-dir", type=Path, help="Full Xcode Contents/Developer directory"
    )
    parser.add_argument("--build-only", action="store_true", help="Skip tests")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    native = root / "native/macos-network"
    output = root / "output/macos-network-check"
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    if args.developer_dir:
        env["DEVELOPER_DIR"] = str(args.developer_dir.resolve())
    developer = (
        env.get("DEVELOPER_DIR")
        or subprocess.check_output(["xcode-select", "-p"], text=True).strip()
    )
    if not (Path(developer) / "Platforms/MacOSX.platform").is_dir():
        parser.error(
            "Full Xcode is required; pass --developer-dir /Applications/Xcode.app/Contents/Developer"
        )
    if not shutil.which("xcodegen"):
        parser.error("xcodegen is required to generate the project")

    def run(name, command):
        path = output / f"{name}.log"
        print(f"{name}: {path}", flush=True)
        with path.open("w") as log:
            result = subprocess.run(
                command,
                cwd=root,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                check=False,
            )
        if result.returncode:
            print(path.read_text()[-12000:], file=sys.stderr)
            raise SystemExit(result.returncode)

    run("generate", ["xcodegen", "generate", "--spec", str(native / "project.yml")])
    common = [
        "xcrun",
        "xcodebuild",
        "-project",
        str(native / "TreerNetwork.xcodeproj"),
        "-configuration",
        "Debug",
        "-derivedDataPath",
        str(output / "xcode"),
        "CODE_SIGNING_ALLOWED=NO",
    ]
    if not args.build_only:
        run(
            "transport-tests",
            [
                "xcrun",
                "swift",
                "test",
                "--package-path",
                str(native),
                "--scratch-path",
                str(output / "swift"),
            ],
        )
        run("identity-tests", common + ["-scheme", "TreerNetworkPlatformTests", "test"])
    run("build", common + ["-scheme", "TreerNetwork", "build"])
    app = output / "xcode/Build/Products/Debug/TreerNetwork.app"
    extensions = list(
        (app / "Contents/Library/SystemExtensions").glob("*.systemextension")
    )
    if len(extensions) != 1:
        raise SystemExit(
            "App must embed exactly one system extension in Contents/Library/SystemExtensions"
        )
    with (extensions[0] / "Contents/Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if info.get("CFBundleIdentifier") != "org.treer.network.extension":
        raise SystemExit("Unexpected extension bundle identity")
    providers = info.get("NetworkExtension", {}).get("NEProviderClasses", {})
    if (
        providers.get("com.apple.networkextension.app-proxy")
        != "TreerNetworkExtension.TransparentProxyProvider"
    ):
        raise SystemExit("Extension provider class is not configured correctly")
    print(f"PASS: unsigned app structure verified: {app}")
    print(
        "No installation, signing, DNS, routes or system-extension settings were changed."
    )


if __name__ == "__main__":
    main()
