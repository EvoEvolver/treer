# Treer iOS

Native fleet orchestrator for a self-hosted Treer Proxy. Thread rendering
loads bundled `mobile/agent-ui/` in a WKWebView and forwards HTTP/WebSocket
to the Agent AIS tunnel. Login is email/password with `X-Treer-Client:
mobile_ios` and a Keychain Bearer session.

## Generate and build

```sh
export DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer
export PATH="$DEVELOPER_DIR/usr/bin:/opt/homebrew/bin:$PATH"
cd mobile/ios
xcodegen generate
xcodebuild -project Treer.xcodeproj -scheme Treer \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=27.0' \
  -configuration Debug build CODE_SIGNING_ALLOWED=NO
```

From the repo root: `just mobile-ios-ci`.

The generated `Treer.xcodeproj` is checked in so CI can build without
xcodegen, but regenerate after changing `project.yml`.

## UI tests

Launch argument `-treer-reset` clears local session state.
Launch argument `-treer-fixture` skips the live Proxy and loads a snapshot
fixture after Proxy Setup.
