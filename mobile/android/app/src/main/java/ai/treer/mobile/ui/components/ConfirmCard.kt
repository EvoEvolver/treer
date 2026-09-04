package ai.treer.mobile.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import ai.treer.mobile.domain.ConfirmSpec

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConfirmCard(
    spec: ConfirmSpec,
    onConfirm: () -> Unit,
    onChange: () -> Unit,
    onCancel: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onCancel,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp)
                .padding(bottom = 28.dp)
                .semantics { contentDescription = "confirm-card" },
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(spec.title, style = MaterialTheme.typography.titleLarge)
            Text(spec.objectName, style = MaterialTheme.typography.titleMedium)
            spec.machineHostname?.takeIf { it.isNotBlank() }?.let {
                Text("Machine · $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            spec.objectIdSuffix?.takeIf { it.isNotBlank() }?.let {
                Text("ID · $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            spec.promptExcerpt?.takeIf { it.isNotBlank() }?.let {
                Text(it, style = MaterialTheme.typography.bodyMedium)
            }
            Text(spec.consequence, style = MaterialTheme.typography.bodyMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                Button(
                    onClick = onConfirm,
                    modifier = Modifier
                        .weight(1f)
                        .testTag("confirm-action"),
                ) { Text("Confirm") }
                if (spec.showChange) {
                    OutlinedButton(onClick = onChange, modifier = Modifier.weight(1f)) { Text("Change") }
                }
            }
            TextButton(onClick = onCancel, modifier = Modifier.fillMaxWidth()) { Text("Cancel") }
        }
    }
}
