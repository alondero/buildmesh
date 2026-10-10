package dev.buildmesh.remote

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em

@Composable
fun BuildmeshWordmark(modifier: Modifier = Modifier, markSize: Dp = 32.dp) {
    Row(
        modifier.semantics(mergeDescendants = true) { contentDescription = "Buildmesh" },
        horizontalArrangement = Arrangement.spacedBy(markSize * 0.3f),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Image(painterResource(R.drawable.relay_mark), contentDescription = null, modifier = Modifier.size(markSize))
        Text(
            buildAnnotatedString {
                withStyle(SpanStyle(color = MeshColors.text)) { append("build") }
                withStyle(SpanStyle(color = MeshColors.accent)) { append("mesh") }
            },
            fontFamily = Geist, fontWeight = FontWeight.ExtraBold,
            // The wordmark is artwork: preserve its proportions at every system font scale.
            fontSize = with(LocalDensity.current) { (markSize * (42f / 96f)).toSp() },
            letterSpacing = (-0.03).em,
            maxLines = 1,
        )
    }
}
