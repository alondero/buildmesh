package dev.buildmesh.remote

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.mutableStateOf

class MainActivity : ComponentActivity() {
    private val vm by viewModels<BuildmeshViewModel>()
    private val invitation = mutableStateOf("")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        receiveInvitation(intent)
        setContent { BuildmeshTheme { BuildmeshApp(vm, invitation.value) } }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        receiveInvitation(intent)
    }

    private fun receiveInvitation(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme == "buildmesh" && uri.host == "pair") invitation.value = uri.getQueryParameter("url").orEmpty()
        // Tickets never survive in the Activity intent after delivery.
        intent.data = null
    }
    override fun onResume() { super.onResume(); vm.resume() }
    override fun onPause() { vm.pause(); super.onPause() }
}
