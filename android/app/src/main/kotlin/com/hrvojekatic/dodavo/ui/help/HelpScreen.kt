package com.hrvojekatic.dodavo.ui.help

import android.content.Intent
import android.view.ViewGroup
import android.webkit.RenderProcessGoneDetail
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.viewinterop.AndroidView
import androidx.webkit.WebViewAssetLoader
import com.hrvojekatic.dodavo.R
import com.hrvojekatic.dodavo.ui.Navigator
import com.hrvojekatic.dodavo.ui.common.startOrTell

/** The help in the app's language (assets/help, from res/help/android). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HelpScreen(navigator: Navigator) {
    val language = stringResource(R.string.dodavo_lang)
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = navigator::back) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.navigate_up))
                    }
                },
                title = { Text(stringResource(R.string.help_title)) },
            )
        },
    ) { padding ->
        AndroidView(
            modifier = Modifier.fillMaxSize().padding(padding),
            factory = { context ->
                val assets = WebViewAssetLoader.Builder()
                    .addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(context))
                    .build()
                WebView(context).apply {
                    settings.javaScriptEnabled = false
                    webViewClient = object : WebViewClient() {
                        override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
                            assets.shouldInterceptRequest(request.url)

                        /** Android ended the page's renderer (to free memory, say): back to the app. */
                        override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                            (view.parent as? ViewGroup)?.removeView(view)
                            view.destroy()
                            navigator.back()
                            return true
                        }

                        /** Other sites open in the browser. */
                        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                            if (request.url.host == "appassets.androidplatform.net") return false
                            context.startOrTell(Intent(Intent.ACTION_VIEW, request.url), R.string.no_app_for_link)
                            return true
                        }
                    }
                    loadUrl("https://appassets.androidplatform.net/assets/help/$language.html")
                }
            },
        )
    }
}
