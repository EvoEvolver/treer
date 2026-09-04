package ai.treer.mobile.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import ai.treer.mobile.ui.AppViewModel
import ai.treer.mobile.ui.components.ErrorBanner

@Composable
fun WorkspaceSwitcherScreen(vm: AppViewModel) {
    var newName by rememberSaveable { mutableStateOf("") }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(20.dp)
            .testTag("workspace-switcher"),
    ) {
        Text(
            "Organization first, then a workspace. All fleet actions stay in that workspace.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(bottom = 12.dp),
        )
        ErrorBanner(vm.error)
        if (vm.organizations.isEmpty()) {
            Text("No organizations yet. You need an invitation or a desktop-created org.")
        } else {
            Text("Organizations", style = MaterialTheme.typography.titleSmall)
            LazyColumn(modifier = Modifier.weight(1f, fill = false)) {
                items(vm.organizations, key = { it.organizationId }) { org ->
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { vm.selectOrganization(org) }
                            .padding(vertical = 10.dp)
                            .testTag("organization-${org.name}"),
                    ) {
                        Text(org.name, style = MaterialTheme.typography.bodyLarge)
                        Text(org.role, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
        vm.selectedOrg?.let { org ->
            Text("Workspaces in ${org.name}", style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(top = 16.dp))
            if (vm.workspaces.isEmpty()) {
                Text("No workspaces yet. Create one to start.")
            } else {
                vm.workspaces.forEach { workspace ->
                    Text(
                        workspace.name,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { vm.selectWorkspace(workspace) }
                            .padding(vertical = 10.dp)
                            .testTag("workspace-${workspace.name}"),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
            }
            Column(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 16.dp)) {
                OutlinedTextField(
                    value = newName,
                    onValueChange = { newName = it },
                    label = { Text("New workspace name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Button(
                    onClick = { vm.createWorkspace(newName.trim()); newName = "" },
                    enabled = newName.isNotBlank() && !vm.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Create workspace") }
            }
        }
    }
}
