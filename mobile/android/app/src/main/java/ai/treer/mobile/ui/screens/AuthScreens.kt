package ai.treer.mobile.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import ai.treer.mobile.ui.AppViewModel
import ai.treer.mobile.ui.AppScreen
import ai.treer.mobile.ui.components.ErrorBanner

@Composable
fun ProxySetupScreen(vm: AppViewModel) {
    var url by rememberSaveable { mutableStateOf(vm.proxyDraft.ifBlank { vm.proxyUrl }) }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Treer", style = MaterialTheme.typography.headlineMedium)
        Text(
            "This is the control plane address, not a Mac's LAN IP. Paste the Proxy URL you already deployed.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedTextField(
            value = url,
            onValueChange = { url = it },
            label = { Text("Proxy URL") },
            placeholder = { Text("https://proxy.example.com") },
            singleLine = true,
            modifier = Modifier
                .fillMaxWidth()
                .testTag("proxy-url-field"),
        )
        ErrorBanner(vm.error)
        Button(
            onClick = { vm.continueProxy(url) },
            enabled = !vm.busy && url.isNotBlank(),
            modifier = Modifier
                .fillMaxWidth()
                .testTag("proxy-continue"),
        ) {
            Text(if (vm.busy) "Checking…" else "Continue")
        }
    }
}

@Composable
fun LoginScreen(vm: AppViewModel) {
    var email by rememberSaveable { mutableStateOf(vm.user?.email.orEmpty()) }
    var password by rememberSaveable { mutableStateOf("") }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Sign in", style = MaterialTheme.typography.headlineMedium)
        Text(vm.baseUrl, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        if (vm.authConfig?.invitationRequired == true) {
            Text("This Proxy requires an invitation to register.", style = MaterialTheme.typography.bodySmall)
        }
        OutlinedTextField(
            value = email,
            onValueChange = { email = it },
            label = { Text("Email") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email),
            modifier = Modifier
                .fillMaxWidth()
                .testTag("login-email"),
        )
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text("Password") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier
                .fillMaxWidth()
                .testTag("login-password"),
        )
        ErrorBanner(vm.error)
        Button(
            onClick = { vm.login(email.trim(), password) },
            enabled = !vm.busy && email.isNotBlank() && password.isNotBlank(),
            modifier = Modifier
                .fillMaxWidth()
                .testTag("login-submit"),
        ) {
            Text(if (vm.busy) "Signing in…" else "Sign in")
        }
        TextButton(onClick = { vm.go(AppScreen.ForgotPassword) }) { Text("Forgot password") }
        TextButton(onClick = { vm.go(AppScreen.Register) }) { Text("Create account") }
        TextButton(onClick = { vm.go(AppScreen.ProxySetup, push = false) }) { Text("Change Proxy URL") }
    }
}

@Composable
fun RegisterScreen(vm: AppViewModel) {
    var email by rememberSaveable { mutableStateOf("") }
    var name by rememberSaveable { mutableStateOf("") }
    var password by rememberSaveable { mutableStateOf("") }
    var invite by rememberSaveable { mutableStateOf("") }
    val inviteRequired = vm.authConfig?.invitationRequired == true
    Column(
        modifier = Modifier
            .fillMaxSize()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Create account", style = MaterialTheme.typography.headlineMedium)
        if (inviteRequired) {
            Text(
                "This Proxy requires an invitation. Ask an administrator or use a desktop invite.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        OutlinedTextField(value = name, onValueChange = { name = it }, label = { Text("Preferred name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(value = email, onValueChange = { email = it }, label = { Text("Email") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text("Password") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(value = invite, onValueChange = { invite = it }, label = { Text("Invitation code") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        ErrorBanner(vm.error)
        Button(
            onClick = { vm.register(email.trim(), name.trim().ifBlank { email.substringBefore("@") }, password, invite.ifBlank { null }) },
            enabled = !vm.busy && email.isNotBlank() && password.isNotBlank() && (!inviteRequired || invite.isNotBlank()),
            modifier = Modifier.fillMaxWidth(),
        ) { Text(if (vm.busy) "Creating…" else "Register") }
        TextButton(onClick = { vm.go(AppScreen.Login, push = false) }) { Text("Back to sign in") }
    }
}

@Composable
fun ForgotPasswordScreen(vm: AppViewModel) {
    var email by rememberSaveable { mutableStateOf("") }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .imePadding()
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Reset password", style = MaterialTheme.typography.headlineMedium)
        OutlinedTextField(value = email, onValueChange = { email = it }, label = { Text("Email") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        ErrorBanner(vm.error)
        Button(
            onClick = { vm.requestPasswordReset(email.trim()) },
            enabled = !vm.busy && email.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Request reset") }
        TextButton(onClick = { vm.go(AppScreen.ResetPassword) }) { Text("I already have a reset token") }
        TextButton(onClick = { vm.go(AppScreen.Login, push = false) }) { Text("Back to sign in") }
    }
}

@Composable
fun ResetPasswordScreen(vm: AppViewModel) {
    var token by rememberSaveable { mutableStateOf(vm.resetToken) }
    var password by rememberSaveable { mutableStateOf("") }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .imePadding()
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Set a new password", style = MaterialTheme.typography.headlineMedium)
        OutlinedTextField(value = token, onValueChange = { token = it }, label = { Text("Reset token") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text("New password") },
            visualTransformation = PasswordVisualTransformation(),
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        ErrorBanner(vm.error)
        Button(
            onClick = { vm.resetPassword(token.trim(), password) },
            enabled = !vm.busy && token.isNotBlank() && password.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Update password") }
        Spacer(Modifier.height(8.dp))
        TextButton(onClick = { vm.go(AppScreen.Login, push = false) }) { Text("Back to sign in") }
    }
}
