package dev.buildmesh.remote

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf

class MainActivity : ComponentActivity() {
    private val vm by viewModels<BuildmeshViewModel>()
    private val invitation = mutableStateOf("")
    private val requestedNode = mutableLongStateOf(0L)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        receiveInvitation(intent)
        setContent {
            BuildmeshTheme {
                BuildmeshApp(vm, invitation.value, requestedNode.longValue) { requestedNode.longValue = 0L }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        receiveInvitation(intent)
    }

    private fun receiveInvitation(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme == "buildmesh" && uri.host == "pair") invitation.value = uri.getQueryParameter("url").orEmpty()
        // A notification names the node it is about, so tapping it opens that
        // node rather than the dashboard the phone was left on.
        val nodeId = nodeIdFromLink(uri.toString())
        if (nodeId > 0L) requestedNode.longValue = nodeId
        // Tickets never survive in the Activity intent after delivery.
        intent.data = null
    }
    override fun onResume() { super.onResume(); vm.resume() }
    override fun onPause() { vm.pause(); super.onPause() }
}
