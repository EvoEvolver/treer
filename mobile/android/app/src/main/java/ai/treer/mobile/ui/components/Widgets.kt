package ai.treer.mobile.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import ai.treer.mobile.domain.Agent
import ai.treer.mobile.domain.Machine
import ai.treer.mobile.domain.agentSummary
import java.time.Instant
import java.time.OffsetDateTime
import java.time.format.DateTimeParseException

@Composable
fun StatusDot(status: String, modifier: Modifier = Modifier) {
    val color = when (status.lowercase()) {
        "online", "idle" -> Color(0xFF22C55E)
        "starting", "working" -> Color(0xFF38BDF8)
        "blocked", "unknown" -> Color(0xFFF59E0B)
        "failed", "exited", "offline" -> Color(0xFFEF4444)
        else -> Color(0xFF9CA3AF)
    }
    Box(
        modifier = modifier
            .size(8.dp)
            .clip(CircleShape)
            .background(color)
            .semantics { contentDescription = status },
    )
}

@Composable
fun ErrorBanner(message: String?) {
    if (message.isNullOrBlank()) return
    Text(
        text = message,
        color = MaterialTheme.colorScheme.error,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    )
}

@Composable
fun SectionHeader(title: String, trailing: String? = null) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = MaterialTheme.typography.titleSmall)
        if (!trailing.isNullOrBlank()) {
            Text(trailing, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
fun AgentRow(
    agent: Agent,
    machine: Machine?,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val status = agent.displayStatus(machine)
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(status)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(agent.name, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                agentSummary(agent, machine),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                listOfNotNull(agent.kind, machine?.displayName, relativeTime(agent.updatedAt)).joinToString(" · "),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
fun CapabilityBadge(label: String, enabled: Boolean) {
    val color = if (enabled) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outline
    Text(
        text = label,
        color = if (enabled) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        style = MaterialTheme.typography.labelSmall,
        modifier = Modifier
            .clip(RoundedCornerShape(999.dp))
            .background(color.copy(alpha = 0.12f))
            .padding(horizontal = 8.dp, vertical = 4.dp)
            .semantics { contentDescription = if (enabled) label else "$label unavailable" },
    )
}

@Composable
fun MonoBlock(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        fontFamily = FontFamily.Monospace,
        style = MaterialTheme.typography.bodySmall,
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(10.dp),
    )
}

fun tagged(tag: String): Modifier = Modifier.testTag(tag)

fun relativeTime(iso: String?): String? {
    if (iso.isNullOrBlank()) return null
    val instant = parseInstant(iso) ?: return null
    val seconds = ((System.currentTimeMillis() - instant.toEpochMilli()) / 1000).coerceAtLeast(0)
    return when {
        seconds < 60 -> "updated ${seconds}s ago"
        seconds < 3600 -> "updated ${seconds / 60}m ago"
        seconds < 86400 -> "updated ${seconds / 3600}h ago"
        else -> "updated ${seconds / 86400}d ago"
    }
}

private fun parseInstant(iso: String): Instant? {
    return try {
        Instant.parse(iso)
    } catch (_: DateTimeParseException) {
        try {
            OffsetDateTime.parse(iso).toInstant()
        } catch (_: Exception) {
            null
        }
    }
}
