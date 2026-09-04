import XCTest

final class TreerUITests: XCTestCase {
    @MainActor
    func testProxySetupLaunchAndURLField() {
        let app = XCUIApplication()
        app.launchArguments = ["-treer-reset"]
        app.launch()

        let urlField = app.textFields["proxy-url-field"]
        XCTAssertTrue(urlField.waitForExistence(timeout: 8))
        urlField.tap()
        urlField.typeText("http://127.0.0.1:8787")
        XCTAssertTrue(app.buttons["proxy-continue"].exists)
        XCTAssertEqual(urlField.value as? String, "http://127.0.0.1:8787")
    }

    @MainActor
    func testFixtureExposesMainIdentifiers() {
        let app = XCUIApplication()
        app.launchArguments = ["-treer-reset", "-treer-fixture"]
        app.launch()

        let urlField = app.textFields["proxy-url-field"]
        XCTAssertTrue(urlField.waitForExistence(timeout: 8))
        urlField.tap()
        urlField.typeText("http://127.0.0.1:8787")
        app.buttons["proxy-continue"].tap()

        let email = app.textFields["login-email"]
        XCTAssertTrue(email.waitForExistence(timeout: 8))
        email.tap()
        email.typeText("operator@treer.test")
        let password = app.secureTextFields["login-password"]
        XCTAssertTrue(password.exists)
        password.tap()
        password.typeText("password")
        app.buttons["login-submit"].tap()

        let workspace = app.staticTexts["workshop"]
        XCTAssertTrue(workspace.waitForExistence(timeout: 8) || app.buttons["workshop"].waitForExistence(timeout: 8))
        if app.buttons["workshop"].exists {
            app.buttons["workshop"].tap()
        } else {
            workspace.tap()
        }

        XCTAssertTrue(app.descendants(matching: .any)["home-screen"].waitForExistence(timeout: 8))
        XCTAssertTrue(app.buttons["voice-button"].exists)
        XCTAssertTrue(app.buttons["assign-button"].exists)
        XCTAssertTrue(app.buttons["settings-button"].exists)
        XCTAssertTrue(app.buttons["workspace-switcher"].exists)
        let machines = app.tabBars.buttons["machines-tab"].exists
            ? app.tabBars.buttons["machines-tab"]
            : app.tabBars.buttons["Machines"]
        let inbox = app.tabBars.buttons["inbox-tab"].exists
            ? app.tabBars.buttons["inbox-tab"]
            : app.tabBars.buttons["Inbox"]
        XCTAssertTrue(machines.exists)
        XCTAssertTrue(inbox.exists)

        machines.tap()
        let addMachine = app.buttons["add-machine-button"]
        XCTAssertTrue(addMachine.waitForExistence(timeout: 8))
        addMachine.tap()
        XCTAssertTrue(app.descendants(matching: .any)["add-machine-screen"].waitForExistence(timeout: 8))
        XCTAssertTrue(app.buttons["copy-install-command"].exists)
        XCTAssertTrue(app.buttons["copy-connect-command"].exists)
        app.buttons["Close"].tap()

        let home = app.tabBars.buttons["home-tab"].exists
            ? app.tabBars.buttons["home-tab"]
            : app.tabBars.buttons["Home"]
        home.tap()
        XCTAssertTrue(app.buttons["assign-button"].waitForExistence(timeout: 8))
        app.buttons["assign-button"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["create-agent-screen"].waitForExistence(timeout: 8))
        XCTAssertTrue(app.buttons["create-submit"].exists)
    }

    @MainActor
    func testLiveLoginAddMachineCreateCodexAndPrompt() throws {
        let live = ProcessInfo.processInfo.environment["TREER_E2E_LIVE"] == "1"
            || ProcessInfo.processInfo.environment["TEST_RUNNER_TREER_E2E_LIVE"] == "1"
            || FileManager.default.fileExists(atPath: "/tmp/treer-e2e-live")
        try XCTSkipUnless(live, "set TREER_E2E_LIVE=1 (or TEST_RUNNER_TREER_E2E_LIVE) to run against a live Proxy")
        let proxy = ProcessInfo.processInfo.environment["TREER_E2E_PROXY"] ?? "http://192.168.50.100:8788"
        let email = ProcessInfo.processInfo.environment["TREER_E2E_EMAIL"] ?? "phone@treer.test"
        let password = ProcessInfo.processInfo.environment["TREER_E2E_PASSWORD"] ?? "treer-phone-1"
        let workspace = ProcessInfo.processInfo.environment["TREER_E2E_WORKSPACE"] ?? "lab"

        let app = XCUIApplication()
        app.launchArguments = ["-treer-reset"]
        app.launch()

        let urlField = app.textFields["proxy-url-field"]
        XCTAssertTrue(urlField.waitForExistence(timeout: 8))
        urlField.tap()
        urlField.typeText(proxy)
        app.buttons["proxy-continue"].tap()

        let emailField = app.textFields["login-email"]
        XCTAssertTrue(emailField.waitForExistence(timeout: 12))
        emailField.tap()
        emailField.typeText(email)
        let passwordField = app.secureTextFields["login-password"]
        passwordField.tap()
        passwordField.typeText(password)
        app.buttons["login-submit"].tap()

        let org = app.buttons["organization-Mobile Personal"]
        if org.waitForExistence(timeout: 12) {
            org.tap()
        }
        let workspaceButton = app.buttons["workspace-\(workspace)"]
        let workspaceText = app.staticTexts[workspace]
        XCTAssertTrue(
            workspaceButton.waitForExistence(timeout: 16) || workspaceText.waitForExistence(timeout: 16),
            "workspace \(workspace) should appear after login"
        )
        if workspaceButton.exists {
            workspaceButton.tap()
        } else {
            workspaceText.tap()
        }

        XCTAssertTrue(app.descendants(matching: .any)["home-screen"].waitForExistence(timeout: 16))
        let machines = app.tabBars.buttons["Machines"]
        XCTAssertTrue(machines.waitForExistence(timeout: 8))
        machines.tap()
        let addMachine = app.buttons["add-machine-button"]
        XCTAssertTrue(addMachine.waitForExistence(timeout: 8))
        addMachine.tap()
        XCTAssertTrue(app.descendants(matching: .any)["add-machine-screen"].waitForExistence(timeout: 12))
        XCTAssertTrue(app.buttons["copy-connect-command"].waitForExistence(timeout: 12))
        if app.buttons["Close"].waitForExistence(timeout: 4) {
            app.buttons["Close"].tap()
        }

        let home = app.tabBars.buttons["Home"]
        XCTAssertTrue(home.waitForExistence(timeout: 8))
        home.tap()
        let assign = app.buttons["assign-button"]
        XCTAssertTrue(assign.waitForExistence(timeout: 8))
        assign.tap()
        XCTAssertTrue(app.descendants(matching: .any)["create-agent-screen"].waitForExistence(timeout: 12))
        let create = app.buttons["create-submit"]
        XCTAssertTrue(create.waitForExistence(timeout: 8))
        create.tap()
        let confirm = app.buttons["confirm-action"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 8))
        confirm.tap()

        let followUp = app.textFields["follow-up-field"]
        let followUpAny = app.descendants(matching: .any)["follow-up-field"]
        XCTAssertTrue(
            followUp.waitForExistence(timeout: 20) || followUpAny.waitForExistence(timeout: 20) ||
                app.buttons["send-follow-up"].waitForExistence(timeout: 20)
        )
        if followUp.exists {
            followUp.tap()
            followUp.typeText("ping from iOS simulator e2e")
        } else if followUpAny.exists {
            followUpAny.firstMatch.tap()
            app.typeText("ping from iOS simulator e2e")
        }
        let send = app.buttons["send-follow-up"]
        XCTAssertTrue(send.waitForExistence(timeout: 8))
        send.tap()
        XCTAssertTrue(app.descendants(matching: .any)["agent-detail"].waitForExistence(timeout: 8))
    }
}
