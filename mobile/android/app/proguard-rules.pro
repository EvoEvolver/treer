# Keep JavascriptInterface methods used by the Agent UI WebView bridge.
-keepclassmembers class ai.treer.mobile.ui.webview.AgentUiBridge {
    @android.webkit.JavascriptInterface <methods>;
}
