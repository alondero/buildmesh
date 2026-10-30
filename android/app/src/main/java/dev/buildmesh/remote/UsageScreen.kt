package dev.buildmesh.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.serialization.json.*
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

private fun JsonObject.decimal(key: String) = (this[key] as? JsonPrimitive)?.doubleOrNull
private fun amount(value: Double, unit: String) = String.format(Locale.getDefault(), "%.2f %s", value, unit)
private fun usageTime(value: String): String = runCatching {
    DateTimeFormatter.ofLocalizedDateTime(java.time.format.FormatStyle.SHORT)
        .withLocale(Locale.getDefault()).withZone(ZoneId.systemDefault()).format(Instant.parse(value))
}.getOrDefault(value)

/**
 * The desktop's provider usage meters.
 *
 * The list refreshes by pulling down, and the app-bar action stays the
 * shared recovery affordance; a failed read keeps its own Retry, above the
 * fold, so the screen never needs a scroll to recover.
 */
@Composable
fun UsageScreen(api: BuildmeshApi, vm: BuildmeshViewModel) {
    val state by vm.state.collectAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val refresh = remember(api) { ListingRefresh() }
    var rows by remember(api) { mutableStateOf<List<JsonObject>?>(null) }
    var error by remember(api) { mutableStateOf("") }
    var busy by remember(api) { mutableStateOf(true) }
    var started by remember(api) { mutableStateOf(false) }
    LaunchedEffect(api, refresh.reload, lifecycle, state.resourceRevision) {
        // Every restart of this effect is a read the user asked for: the pull
        // moves the screen's own counter and the app-bar action moves the
        // shared resource revision. The first load is not one of them, and the
        // lifecycle alone never restarts this effect.
        var force = started
        started = true
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                busy = true
                try {
                    val fresh = force
                    force = false
                    rows = api.get("/api/usage?refresh=$fresh").objects()
                    error = ""
                    refresh.settle()
                } catch (e: CancellationException) { throw e
                } catch (e: Exception) {
                    if (e is ApiException && e.sessionRejected) { vm.unauthorized(); return@repeatOnLifecycle }
                    error = friendlyError(e)
                    refresh.settle()
                } finally { busy = false }
                force = false
                delay(60_000)
            }
        }
    }
    Column(Modifier.fillMaxSize()) {
        Text("Provider usage on your desktop", modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp))
        if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (error.isNotEmpty()) {
            ErrorBanner(error + if (rows != null) " — showing last known data" else "") { error = "" }
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalArrangement = Arrangement.End) {
                TextButton(enabled = !busy, onClick = { refresh.request() }) { Text("Retry") }
            }
        }
        PullToRefreshList(refresh, Modifier.weight(1f).fillMaxWidth()) {
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (rows == null) EmptyMessage(if (busy) "Loading usage…" else "Could not load usage. Pull down to try again, or use the refresh action in the app bar.")
                else if (rows!!.isEmpty()) EmptyMessage("No usage meters available. Enable providers and sign in on the desktop.")
                rows.orEmpty().forEach { UsageCard(it) }
            }
        }
    }
}

@Composable
private fun UsagePercent(label: String, value: Double?) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(label, modifier = Modifier.weight(1f))
            Text(value?.let { String.format(Locale.getDefault(), "%.1f%%", it) } ?: "Unavailable")
        }
        if (value != null) LinearProgressIndicator(progress = { (value / 100).toFloat().coerceIn(0f, 1f) },
            modifier = Modifier.fillMaxWidth(), color = when { value > 80 -> MeshColors.red; value > 60 -> MeshColors.amber; else -> MeshColors.accent })
    }
}

@Composable
private fun UsageReset(at: String) {
    if (at.isNotEmpty()) Text("Resets: ${usageTime(at)}", style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary)
}

@Composable
private fun UsageAmountRow(label: String, value: Double?, unit: String) {
    if (value != null) Text("$label: ${amount(value, unit)}")
}

@Composable
fun UsageCard(row: JsonObject) {
    val meter = row.objectAt("meter") ?: return
    val usage = meter.objectAt("usage")
    Card(colors = CardDefaults.cardColors(containerColor = MeshColors.surface)) {
        Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(row.text("name"), style = MaterialTheme.typography.titleMedium)
            when {
                !meter.flag("usageTracked") -> Text("Usage not tracked")
                usage == null -> Text("Unable to load usage data")
                usage.text("error").isNotEmpty() -> Text(usage.text("error"), color = MeshColors.red)
                !usage.flag("loggedIn") -> Text("Sign in or update credentials on the desktop.")
                else -> {
                    val windows = usage.arrayAt("windows")
                    windows.forEach { window ->
                        UsagePercent(window.text("label"), window.decimal("usedPercent"))
                        UsageReset(window.text("resetsAt"))
                    }
                    usage.objectAt("balance")?.let { balance ->
                        UsageAmountRow("Balance remaining", balance.decimal("remaining"), balance.text("currency"))
                        UsageAmountRow("Spent this month", balance.decimal("monthlySpend"), balance.text("currency"))
                    }
                    val explicit = usage.arrayAt("meters")
                    explicit.forEach { item ->
                        when (item.text("state")) {
                            "metered", "no_individual_limit" -> item.objectAt("amount")?.let { a ->
                                val unit = a.text("unit")
                                UsageAmountRow("Amount used", a.decimal("used"), unit)
                                if (item.text("state") == "no_individual_limit") Text("No individual limit") else {
                                    UsageAmountRow("Limit", a.decimal("limit"), unit)
                                    UsageAmountRow("Remaining", a.decimal("remaining"), unit)
                                }
                                a.decimal("usedPercent")?.let { UsagePercent("Usage", it) }
                                UsageReset(a.text("resetsAt"))
                            }
                            "unlimited" -> Text("Unlimited")
                            "managed_externally" -> Text("Managed by ${item.text("platform")}")
                            else -> Text("Unavailable")
                        }
                    }
                    if (windows.isEmpty() && explicit.isEmpty() && usage.objectAt("balance") == null) Text("Unavailable")
                    if (usage.text("detail").isNotEmpty()) Text(usage.text("detail"), style = MaterialTheme.typography.bodySmall)
                }
            }
            meter.decimal("cachedAt")?.let { Text("Last known value · ${usageTime(Instant.ofEpochSecond(it.toLong()).toString())}",
                style = MaterialTheme.typography.bodySmall, color = MeshColors.secondary) }
        }
    }
}
