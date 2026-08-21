package org.zerodroid.bridge

import android.accounts.Account
import android.app.Activity
import android.content.Intent
import android.os.Bundle
import com.google.android.gms.auth.GoogleAuthUtil
import com.google.android.gms.auth.UserRecoverableAuthException
import com.google.android.gms.auth.api.signin.GoogleSignIn
import com.google.android.gms.auth.api.signin.GoogleSignInOptions
import com.google.android.gms.common.api.ApiException
import com.google.android.gms.common.api.Scope
import kotlin.concurrent.thread

/**
 * Google Sign-In + access-token fetch for Drive/Gmail/Calendar/Photos.
 * Launched only by the authenticated in-app bridge. The user picks an account and consents; the
 * resulting OAuth access token lands in [Auth] for the localhost API to call Google REST APIs.
 * Handles NEED_REMOTE_CONSENT by launching the recovery consent intent.
 */
class SignInActivity : Activity() {
    private val rcSignIn = 9001
    private val rcConsent = 9002
    private var account: Account? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val b = GoogleSignInOptions.Builder(GoogleSignInOptions.DEFAULT_SIGN_IN)
            .requestEmail().requestProfile()
        for (s in Auth.SCOPES) b.requestScopes(Scope(s))
        startActivityForResult(GoogleSignIn.getClient(this, b.build()).signInIntent, rcSignIn)
    }

    @Deprecated("one-shot consent flow")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        when (requestCode) {
            rcSignIn -> {
                try {
                    val acct = GoogleSignIn.getSignedInAccountFromIntent(data)
                        .getResult(ApiException::class.java)
                    Auth.email = acct.email
                    Auth.displayName = acct.displayName
                    Auth.error = null
                    account = acct.account
                    if (account != null) fetchToken() else finish()
                } catch (e: ApiException) {
                    Auth.error = "signin failed: code ${e.statusCode} ${e.message}"; finish()
                }
            }
            rcConsent -> {
                if (resultCode == RESULT_OK) fetchToken() else { Auth.error = "consent declined"; finish() }
            }
            else -> finish()
        }
    }

    /** Get the OAuth access token; if the scopes need remote consent, launch it and retry. */
    private fun fetchToken() = thread {
        try {
            Auth.accessToken = GoogleAuthUtil.getToken(applicationContext, account!!, Auth.OAUTH_TOKEN_SCOPE)
            Auth.error = null
            runOnUiThread { finish() }
        } catch (e: UserRecoverableAuthException) {
            // NEED_REMOTE_CONSENT — show the consent UI, then onActivityResult(rcConsent) retries.
            runOnUiThread { e.intent?.let { startActivityForResult(it, rcConsent) } ?: finish() }
        } catch (e: Exception) {
            Auth.error = "token: ${e.message}"
            runOnUiThread { finish() }
        }
    }
}
