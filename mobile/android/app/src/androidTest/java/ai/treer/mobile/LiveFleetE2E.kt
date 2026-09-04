package ai.treer.mobile

import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.printToString
import androidx.compose.ui.test.waitUntilExactlyOneExists
import androidx.test.espresso.Espresso
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@OptIn(ExperimentalTestApi::class)
@RunWith(AndroidJUnit4::class)
class LiveFleetE2E {
    @get:Rule
    val rule = createAndroidComposeRule<MainActivity>()

    @Before
    fun resetPrefs() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        context.getSharedPreferences("treer_prefs", 0).edit().clear().commit()
    }

    @Test
    fun loginAddMachineCreateCodexAndPrompt() {
        val proxy = InstrumentationRegistry.getArguments().getString("TREER_E2E_PROXY")
            ?: "http://10.0.2.2:8788"
        val email = InstrumentationRegistry.getArguments().getString("TREER_E2E_EMAIL")
            ?: "phone@treer.test"
        val password = InstrumentationRegistry.getArguments().getString("TREER_E2E_PASSWORD")
            ?: "treer-phone-1"

        waitTag("proxy-url-field", 15_000)
        rule.onNodeWithTag("proxy-url-field").performTextInput(proxy)
        Espresso.closeSoftKeyboard()
        rule.onNodeWithTag("proxy-continue").performClick()

        waitTag("login-email", 20_000)
        rule.onNodeWithTag("login-email").performTextInput(email)
        rule.onNodeWithTag("login-password").performTextInput(password)
        Espresso.closeSoftKeyboard()
        rule.onNodeWithTag("login-submit").performClick()

        rule.waitUntil(25_000) {
            tagged("organization-Mobile Personal") ||
                tagged("home-screen") ||
                rule.onAllNodes(hasText("Mobile Personal", substring = true)).fetchSemanticsNodes().isNotEmpty()
        }
        if (tagged("home-screen")) {
            // last workspace was restored
        } else if (tagged("organization-Mobile Personal")) {
            rule.onNodeWithTag("organization-Mobile Personal").performClick()
            waitTag("workspace-lab", 15_000)
            rule.onNodeWithTag("workspace-lab").performClick()
        } else {
            rule.onNodeWithText("Mobile Personal", substring = true).performClick()
            rule.waitUntil(15_000) {
                tagged("workspace-lab") || rule.onAllNodes(hasText("lab")).fetchSemanticsNodes().isNotEmpty()
            }
            if (tagged("workspace-lab")) {
                rule.onNodeWithTag("workspace-lab").performClick()
            } else {
                rule.onNodeWithText("lab").performClick()
            }
        }

        waitTag("home-screen", 25_000)
        rule.onNodeWithTag("machines-tab").performClick()
        waitTag("add-machine-button", 10_000)
        rule.onNodeWithTag("add-machine-button").performClick()
        waitTag("add-machine-screen", 15_000)
        rule.onNodeWithTag("copy-connect-command").assertIsDisplayed()
        rule.onNodeWithTag("refresh-machines").assertIsDisplayed()

        waitTag("assign-after-enroll", 15_000)
        rule.onNodeWithTag("assign-after-enroll").performClick()
        waitTag("create-kind-codex", 15_000)
        rule.onNodeWithTag("create-kind-codex").performClick()
        rule.onNodeWithTag("create-submit").performScrollTo().performClick()
        waitTag("confirm-action", 10_000)
        rule.onNodeWithTag("confirm-action").performClick()

        waitTag("follow-up-field", 25_000)
        rule.onNodeWithTag("follow-up-field").performTextInput("ping from AOSP e2e")
        Espresso.closeSoftKeyboard()
        rule.onNodeWithTag("send-follow-up").performScrollTo().performClick()
        rule.onNodeWithTag("agent-detail").assertIsDisplayed()
    }

    private fun tagged(tag: String): Boolean =
        rule.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun waitTag(tag: String, timeout: Long) {
        try {
            rule.waitUntilExactlyOneExists(hasTestTag(tag), timeout)
        } catch (error: Throwable) {
            throw AssertionError("missing tag $tag\n${rule.onRoot(useUnmergedTree = true).printToString()}", error)
        }
    }
}
