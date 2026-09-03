package org.zerodroid.bridge

import android.content.Intent

/** Holds the Google Sign-In result + access token so the localhost API can use Google REST APIs. */
object Auth {
    // Workspace + Drive + Photos. Each must be added to the OAuth consent screen and its API
    // enabled in the Google Cloud project, and you must be a Test user (Testing mode).
    val SCOPES = listOf(
        "https://www.googleapis.com/auth/drive.readonly",
        "https://www.googleapis.com/auth/gmail.readonly",
        "https://www.googleapis.com/auth/calendar.readonly",
        "https://www.googleapis.com/auth/photospicker.mediaitems.readonly"
    )
    val OAUTH_TOKEN_SCOPE = "oauth2:" + SCOPES.joinToString(" ")

    @Volatile var email: String? = null
    @Volatile var displayName: String? = null
    @Volatile var accessToken: String? = null
    @Volatile var error: String? = null
    /** Picker session id (Photos Picker API). */
    @Volatile var pickerSessionId: String? = null
}
